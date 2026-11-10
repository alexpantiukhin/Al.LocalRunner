use crate::engine::{BrokerEvent, Event, Shared};
use lr_abstractions::{Frame, Level, Transport};
use lr_core::Zone;
use lr_proto::{
    receive, BrokerToHub, Hello, HubToBroker, PeerKind, Received, HEARTBEAT_INTERVAL_S,
    MISSED_HEARTBEATS_OFFLINE, PROTOCOL_VERSION,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Notify};

pub(crate) const OUT_FRAMES: usize = 1024;

const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

pub(crate) fn heartbeat() -> Duration {
    Duration::from_secs(HEARTBEAT_INTERVAL_S)
}

pub(crate) fn silence_limit() -> Duration {
    Duration::from_secs(HEARTBEAT_INTERVAL_S.saturating_mul(u64::from(MISSED_HEARTBEATS_OFFLINE)))
}

pub(crate) const STREAM_FRAMES: usize = 1024;

type Stream = (String, mpsc::Sender<BrokerToHub>);

#[derive(Default)]
pub(crate) struct Pending {
    waiting: Mutex<HashMap<u64, oneshot::Sender<BrokerToHub>>>,
    streams: Mutex<HashMap<u64, Stream>>,
}

impl Pending {
    pub(crate) fn stream(&self, req: u64, broker: &str) -> mpsc::Receiver<BrokerToHub> {
        let (tx, rx) = mpsc::channel(STREAM_FRAMES);
        if let Ok(mut s) = self.streams.lock() {
            s.insert(req, (broker.to_string(), tx));
        }
        rx
    }

    pub(crate) fn forget_stream(&self, req: u64) {
        if let Ok(mut s) = self.streams.lock() {
            s.remove(&req);
        }
    }

    fn close_streams(&self, broker: &str) {
        if let Ok(mut s) = self.streams.lock() {
            s.retain(|_, (b, _)| b != broker);
        }
    }

    fn feed(&self, req: u64, msg: BrokerToHub) -> Option<BrokerToHub> {
        let Ok(mut s) = self.streams.lock() else {
            return Some(msg);
        };
        let Some((_, tx)) = s.get(&req) else {
            return Some(msg);
        };
        if tx.try_send(msg).is_err() {
            s.remove(&req);
        }
        None
    }

    pub(crate) fn register(&self, req: u64) -> oneshot::Receiver<BrokerToHub> {
        let (tx, rx) = oneshot::channel();
        if let Ok(mut w) = self.waiting.lock() {
            w.insert(req, tx);
        }
        rx
    }

    pub(crate) fn forget(&self, req: u64) {
        if let Ok(mut w) = self.waiting.lock() {
            w.remove(&req);
        }
    }

    fn deliver(&self, req: u64, msg: BrokerToHub) {
        let waiter = self.waiting.lock().ok().and_then(|mut w| w.remove(&req));
        if let Some(tx) = waiter {
            let _ = tx.send(msg);
        }
    }
}

#[derive(Clone)]
pub(crate) struct Target {
    pub(crate) name: String,
    pub(crate) address: String,
    pub(crate) fingerprint: Option<String>,
}

pub(crate) fn spawn(target: Target, shared: Arc<Shared>, events: mpsc::Sender<Event>) {
    tokio::spawn(async move { run(target, shared, events).await });
}

async fn run(target: Target, shared: Arc<Shared>, events: mpsc::Sender<Event>) {
    let mut backoff = BACKOFF_MIN;
    loop {
        if events.is_closed() {
            return;
        }
        match shared
            .dialer
            .dial(&target.address, target.fingerprint.as_deref())
            .await
        {
            Ok(t) => {
                let t: Arc<dyn Transport> = Arc::from(t);
                if session(&target, &shared, &events, t).await {
                    backoff = BACKOFF_MIN;
                }
            }
            Err(e) => shared.diag(
                Level::Debug,
                "broker unreachable",
                &[("broker", &target.name), ("error", &e.to_string())],
            ),
        }
        shared.clock.sleep(backoff).await;
        backoff = backoff.saturating_mul(2).min(BACKOFF_MAX);
    }
}

pub(crate) async fn send_within(shared: &Shared, t: &dyn Transport, frame: Frame) -> bool {
    tokio::select! {
        r = t.send(frame) => r.is_ok(),
        _ = shared.clock.sleep(silence_limit()) => false,
    }
}

async fn first_frame(shared: &Shared, t: &dyn Transport) -> Option<BrokerToHub> {
    let frame = tokio::select! {
        r = t.recv() => r.ok()?,
        _ = shared.clock.sleep(silence_limit()) => return None,
    };
    match receive::<BrokerToHub>(frame.as_bytes(), PeerKind::Network) {
        Ok(Received::Message(m)) => Some(m),
        _ => None,
    }
}

async fn session(
    target: &Target,
    shared: &Arc<Shared>,
    events: &mpsc::Sender<Event>,
    t: Arc<dyn Transport>,
) -> bool {
    let hello = HubToBroker::Hello(Hello {
        version: PROTOCOL_VERSION,
        name: shared.name.clone(),
        capabilities: None,
    });
    let Ok(frame) = Frame::json(&hello) else {
        return false;
    };
    if !send_within(shared, t.as_ref(), frame).await {
        t.close().await;
        return false;
    }
    let zone: Option<Zone> = match first_frame(shared, t.as_ref()).await {
        Some(BrokerToHub::Welcome(w)) => w.zone,
        Some(BrokerToHub::Error(e)) => {
            shared.diag(
                Level::Warn,
                "broker refused the hub",
                &[("broker", &target.name), ("error", &e.message)],
            );
            let _ = events
                .send(Event::Broker(target.name.clone(), BrokerEvent::Refused(e)))
                .await;
            t.close().await;
            return false;
        }
        _ => {
            t.close().await;
            return false;
        }
    };
    let (out_tx, out_rx) = mpsc::channel(OUT_FRAMES);
    let kill = Arc::new(Notify::new());
    let connected = BrokerEvent::Connected {
        zone,
        out: out_tx,
        kill: kill.clone(),
    };
    if events
        .send(Event::Broker(target.name.clone(), connected))
        .await
        .is_err()
    {
        return true;
    }
    tokio::select! {
        _ = writer(shared, t.as_ref(), out_rx) => {}
        _ = reader(target, shared, events, t.as_ref()) => {}
        _ = kill.notified() => {}
    }
    shared.pending.close_streams(&target.name);
    t.close().await;
    let _ = events
        .send(Event::Broker(
            target.name.clone(),
            BrokerEvent::Disconnected,
        ))
        .await;
    true
}

async fn writer(shared: &Shared, t: &dyn Transport, mut out: mpsc::Receiver<Frame>) {
    loop {
        let frame = tokio::select! {
            f = out.recv() => match f {
                Some(f) => f,
                None => return,
            },
            _ = shared.clock.sleep(heartbeat()) => match Frame::json(&HubToBroker::Heartbeat) {
                Ok(f) => f,
                Err(_) => return,
            },
        };
        if !send_within(shared, t, frame).await {
            shared.diag(Level::Warn, "broker stopped reading, reconnecting", &[]);
            return;
        }
    }
}

async fn reader(target: &Target, shared: &Shared, events: &mpsc::Sender<Event>, t: &dyn Transport) {
    loop {
        let frame = tokio::select! {
            r = t.recv() => match r {
                Ok(f) => f,
                Err(_) => return,
            },
            _ = shared.clock.sleep(silence_limit()) => {
                shared.diag(Level::Warn, "broker silent, reconnecting", &[("broker", &target.name)]);
                return;
            }
        };
        let msg = match receive::<BrokerToHub>(frame.as_bytes(), PeerKind::Network) {
            Ok(Received::Message(m)) => m,
            Ok(Received::Ignored(_)) => continue,
            Err(e) => {
                shared.diag(
                    Level::Warn,
                    "message from broker refused",
                    &[("broker", &target.name), ("error", &e.to_string())],
                );
                continue;
            }
        };
        match msg {
            BrokerToHub::CapabilitiesReply(r) => {
                let req = r.req;
                shared
                    .pending
                    .deliver(req, BrokerToHub::CapabilitiesReply(r));
            }
            BrokerToHub::ReadReply(a) => {
                let req = a.req;
                if let Some(msg) = shared.pending.feed(req, BrokerToHub::ReadReply(a)) {
                    shared.pending.deliver(req, msg);
                }
            }
            BrokerToHub::LogChunk(l) => {
                let req = l.req;
                shared.pending.feed(req, BrokerToHub::LogChunk(l));
            }
            BrokerToHub::Heartbeat
            | BrokerToHub::Event(_)
            | BrokerToHub::Welcome(_)
            | BrokerToHub::Joined(_) => {}
            other => {
                let event =
                    Event::Broker(target.name.clone(), BrokerEvent::Message(Box::new(other)));
                if events.send(event).await.is_err() {
                    return;
                }
            }
        }
    }
}
