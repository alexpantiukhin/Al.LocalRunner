use crate::clock::Clock;
use crate::transport::{Dialer, Frame, Listener, Peer, Transport, TransportError, MAX_FRAME_BYTES};
use async_trait::async_trait;
use lr_core::Timestamp;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::Notify;

struct Envelope {
    frame: Frame,
    deliver_at: Option<Timestamp>,
}

#[derive(Default)]
struct Direction {
    queue: Mutex<VecDeque<Envelope>>,
    notify: Notify,
}

#[derive(Default)]
struct Link {
    closed: AtomicBool,
    dirs: [Direction; 2],
}

impl Link {
    fn disconnect(&self) {
        self.closed.store(true, Ordering::SeqCst);
        for dir in &self.dirs {
            dir.queue.lock().unwrap().clear();
            dir.notify.notify_one();
            dir.notify.notify_waiters();
        }
    }
}

#[derive(Default)]
struct FaultPlan {
    drop: usize,
    duplicate: usize,
    delay: usize,
    delay_by: Duration,
}

/// Faults injected into the frames one end sends, and the connection's disconnect switch.
pub struct Faults {
    link: Arc<Link>,
    plan: Mutex<FaultPlan>,
}

impl Faults {
    /// The next `n` frames sent are lost.
    pub fn drop_next(&self, n: usize) {
        self.plan.lock().unwrap().drop += n;
    }

    /// The next `n` frames sent arrive twice.
    pub fn duplicate_next(&self, n: usize) {
        self.plan.lock().unwrap().duplicate += n;
    }

    /// The next `n` frames sent arrive only once the clock has advanced by `by`; later frames
    /// wait behind them, so order is kept.
    pub fn delay_next(&self, n: usize, by: Duration) {
        let mut plan = self.plan.lock().unwrap();
        plan.delay += n;
        plan.delay_by = by;
    }

    /// Breaks the connection: frames in flight are lost and both ends see `Closed`.
    pub fn disconnect(&self) {
        self.link.disconnect();
    }
}

/// One end of an in-memory connection made by [`channel_pair`] or [`FakeNetwork`].
pub struct ChannelTransport {
    peer: Peer,
    link: Arc<Link>,
    send_dir: usize,
    faults: Faults,
    clock: Arc<dyn Clock>,
}

impl ChannelTransport {
    /// Faults of the frames this end sends.
    pub fn faults(&self) -> &Faults {
        &self.faults
    }
}

/// Two connected transports. The first end has identity `a` and sees `b` as its peer; the
/// second the other way round. Delays are measured on `clock`.
pub fn channel_pair(
    clock: Arc<dyn Clock>,
    a: Peer,
    b: Peer,
) -> (ChannelTransport, ChannelTransport) {
    let link = Arc::new(Link::default());
    let end = |peer: Peer, send_dir: usize| ChannelTransport {
        peer,
        link: link.clone(),
        send_dir,
        faults: Faults {
            link: link.clone(),
            plan: Mutex::new(FaultPlan::default()),
        },
        clock: clock.clone(),
    };
    (end(b, 0), end(a, 1))
}

#[async_trait]
impl Transport for ChannelTransport {
    fn peer(&self) -> &Peer {
        &self.peer
    }

    async fn send(&self, frame: Frame) -> Result<(), TransportError> {
        if self.link.closed.load(Ordering::SeqCst) {
            return Err(TransportError::Closed);
        }
        if frame.len() > MAX_FRAME_BYTES {
            return Err(TransportError::TooLarge { len: frame.len() });
        }
        let (copies, deliver_at) = {
            let mut plan = self.faults.plan.lock().unwrap();
            if plan.drop > 0 {
                plan.drop -= 1;
                return Ok(());
            }
            let deliver_at = if plan.delay > 0 {
                plan.delay -= 1;
                Some(Timestamp(
                    self.clock.now().0 + plan.delay_by.as_millis() as u64,
                ))
            } else {
                None
            };
            let copies = if plan.duplicate > 0 {
                plan.duplicate -= 1;
                2
            } else {
                1
            };
            (copies, deliver_at)
        };
        let dir = &self.link.dirs[self.send_dir];
        {
            let mut queue = dir.queue.lock().unwrap();
            for _ in 0..copies {
                queue.push_back(Envelope {
                    frame: frame.clone(),
                    deliver_at,
                });
            }
        }
        dir.notify.notify_one();
        Ok(())
    }

    async fn recv(&self) -> Result<Frame, TransportError> {
        let dir = &self.link.dirs[1 - self.send_dir];
        loop {
            if self.link.closed.load(Ordering::SeqCst) {
                return Err(TransportError::Closed);
            }
            let wait_until = {
                let mut queue = dir.queue.lock().unwrap();
                match queue.front() {
                    Some(env) => match env.deliver_at {
                        Some(at) if at > self.clock.now() => Some(at),
                        _ => return Ok(queue.pop_front().unwrap().frame),
                    },
                    None => None,
                }
            };
            match wait_until {
                Some(at) => self.clock.sleep_until(at).await,
                None => dir.notify.notified().await,
            }
        }
    }

    async fn close(&self) {
        self.link.disconnect();
    }
}

struct ListenerState {
    peer: Peer,
    queue: Mutex<VecDeque<ChannelTransport>>,
    notify: Notify,
}

#[derive(Default)]
struct Net {
    listeners: HashMap<String, Arc<ListenerState>>,
    unreachable: HashSet<String>,
    links: HashMap<String, Vec<Weak<Link>>>,
}

/// An in-memory network of named addresses: listeners bind an address, dialers connect to it.
#[derive(Clone)]
pub struct FakeNetwork {
    net: Arc<Mutex<Net>>,
    clock: Arc<dyn Clock>,
}

impl FakeNetwork {
    /// An empty network; delays of its connections are measured on `clock`.
    pub fn new(clock: Arc<dyn Clock>) -> FakeNetwork {
        FakeNetwork {
            net: Arc::new(Mutex::new(Net::default())),
            clock,
        }
    }

    /// Binds `address`; connecting clients see `peer` (its key is the certificate
    /// fingerprint checked by [`Dialer::dial`]).
    pub fn listen(&self, address: &str, peer: Peer) -> FakeListener {
        let state = Arc::new(ListenerState {
            peer,
            queue: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
        });
        self.net
            .lock()
            .unwrap()
            .listeners
            .insert(address.to_string(), state.clone());
        FakeListener { state }
    }

    /// A dialer whose connections the listener sees as coming from `peer`.
    pub fn dialer(&self, peer: Peer) -> FakeDialer {
        FakeDialer {
            network: self.clone(),
            peer,
        }
    }

    /// Makes `address` reachable or not for new connections; live connections stay.
    pub fn set_reachable(&self, address: &str, reachable: bool) {
        let mut net = self.net.lock().unwrap();
        if reachable {
            net.unreachable.remove(address);
        } else {
            net.unreachable.insert(address.to_string());
        }
    }

    /// Breaks every live connection to `address`.
    pub fn cut(&self, address: &str) {
        let links = self
            .net
            .lock()
            .unwrap()
            .links
            .remove(address)
            .unwrap_or_default();
        for link in links.iter().filter_map(Weak::upgrade) {
            link.disconnect();
        }
    }
}

/// Listener bound by [`FakeNetwork::listen`].
pub struct FakeListener {
    state: Arc<ListenerState>,
}

impl FakeListener {
    /// Connections dialed but not accepted yet.
    pub fn pending(&self) -> usize {
        self.state.queue.lock().unwrap().len()
    }
}

#[async_trait]
impl Listener for FakeListener {
    async fn accept(&self) -> Result<Box<dyn Transport>, TransportError> {
        loop {
            if let Some(conn) = self.state.queue.lock().unwrap().pop_front() {
                return Ok(Box::new(conn));
            }
            self.state.notify.notified().await;
        }
    }
}

/// Dialer made by [`FakeNetwork::dialer`].
pub struct FakeDialer {
    network: FakeNetwork,
    peer: Peer,
}

#[async_trait]
impl Dialer for FakeDialer {
    async fn dial(
        &self,
        address: &str,
        fingerprint: Option<&str>,
    ) -> Result<Box<dyn Transport>, TransportError> {
        let mut net = self.network.net.lock().unwrap();
        let listener = match net.listeners.get(address) {
            Some(l) if !net.unreachable.contains(address) => l.clone(),
            _ => return Err(TransportError::Unreachable(address.to_string())),
        };
        if let Some(fp) = fingerprint {
            if listener.peer.key.as_deref() != Some(fp) {
                return Err(TransportError::Refused(format!(
                    "certificate fingerprint of {address} does not match"
                )));
            }
        }
        let (client, server) = channel_pair(
            self.network.clock.clone(),
            self.peer.clone(),
            listener.peer.clone(),
        );
        net.links
            .entry(address.to_string())
            .or_default()
            .push(Arc::downgrade(&client.link));
        listener.queue.lock().unwrap().push_back(server);
        listener.notify.notify_one();
        Ok(Box::new(client))
    }
}
