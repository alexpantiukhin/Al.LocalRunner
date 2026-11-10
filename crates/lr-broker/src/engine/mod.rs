//! The broker engine (spec 13.2): one owner of the queue, the runner registry and the job
//! states, fed by a command channel from the connection tasks; changes go out on a broadcast
//! bus for the read API.

mod assign;
mod conn;
mod events;
mod hello;
mod orders;
mod read;
mod runners;

pub use events::BusEvent;

use crate::policy::{ActivePolicy, ListenValue, LocalUserEntry};
use conn::{Command, ConnId, Out};
use lr_abstractions::{
    BrokerStore, Clock, Frame, JobKey, Listener, Peer, SecretSource, TransportError,
};
use lr_core::{Endpoint, JobId, QueueEntry, ServeLog, Site, Timestamp};
use lr_proto::{OrderSpec, ProtocolError, ProtocolErrorCode, Topic};
use runners::RunnerSlot;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

/// The client record of an admitted local IPC peer (spec 3.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalClient {
    /// Client key: `local:` and the OS user.
    pub key: String,
    /// Label from its `[[local_user]]` entry.
    pub label: String,
    /// Whether the OS user owns the broker (rights on its state directory): it sees every run
    /// and log and may cancel other clients' orders under `[cancel] others = "owner"`.
    pub owner: bool,
}

/// Decides which local IPC peers the broker serves; the OS user comes from the kernel.
pub trait LocalAdmission: Send + Sync {
    /// The client record of `os_user`, `None` when it may not use the broker.
    fn admit(&self, os_user: &str, users: &[LocalUserEntry]) -> Option<LocalClient>;
}

/// Admits the OS users named exactly in `[[local_user]]`, plus the broker owners given at
/// construction.
#[derive(Debug, Clone, Default)]
pub struct ExactLocalUsers {
    owners: BTreeSet<String>,
}

impl ExactLocalUsers {
    /// Admission with these OS users as broker owners.
    pub fn with_owners(owners: impl IntoIterator<Item = String>) -> ExactLocalUsers {
        ExactLocalUsers {
            owners: owners.into_iter().collect(),
        }
    }
}

impl LocalAdmission for ExactLocalUsers {
    fn admit(&self, os_user: &str, users: &[LocalUserEntry]) -> Option<LocalClient> {
        let listed = users.iter().find(|u| u.name == os_user);
        let owner = self.owners.contains(os_user);
        if listed.is_none() && !owner {
            return None;
        }
        Some(LocalClient {
            key: format!("local:{os_user}"),
            label: listed.map(|u| u.label.clone()).unwrap_or_default(),
            owner,
        })
    }
}

/// What the broker is built from; only `lr-cli` knows the implementations behind them.
pub struct BrokerDeps {
    /// Time source.
    pub clock: Arc<dyn Clock>,
    /// `broker.db` and `logs/`.
    pub store: Arc<dyn BrokerStore>,
    /// TLS addresses and local IPC.
    pub listener: Arc<dyn Listener>,
    /// Secret store; the broker never reads secret values (spec 3.13), it only decides which
    /// names a client may request.
    pub secrets: Arc<dyn SecretSource>,
    /// The policy applied at start; the caller has run `ListenSection::check_at_start`.
    pub policy: ActivePolicy,
}

/// The broker role: accepts connections, queues orders and gives them to runners.
pub struct Broker {
    engine: Engine,
    listener: Arc<dyn Listener>,
    tx: mpsc::UnboundedSender<Command>,
    rx: mpsc::UnboundedReceiver<Command>,
}

impl Broker {
    /// A broker over `deps`, admitting local users named exactly in `[[local_user]]`.
    pub fn new(deps: BrokerDeps) -> Broker {
        let (tx, rx) = mpsc::unbounded_channel();
        let file = deps.policy.file();
        let site = Site {
            broker_addrs: file
                .listen
                .addresses
                .iter()
                .filter_map(|a| match a {
                    ListenValue::Explicit(ip, _) => Some(*ip),
                    ListenValue::Container => None,
                })
                .collect(),
            networks: file.networks.clone(),
        };
        Broker {
            engine: Engine {
                clock: deps.clock,
                store: deps.store,
                policy: deps.policy,
                site,
                local: Arc::new(ExactLocalUsers::default()),
                bus: broadcast::channel(1024).0,
                conns: HashMap::new(),
                hubs: HashMap::new(),
                runners: BTreeMap::new(),
                queue: Vec::new(),
                active: Vec::new(),
                served: ServeLog::default(),
                waits: HashMap::new(),
                next_id: 0,
                next_sub: 0,
            },
            listener: deps.listener,
            tx,
            rx,
        }
    }

    /// Replaces how local IPC peers are admitted (the OS-specific account rules live with the
    /// IPC implementation).
    pub fn with_local_admission(mut self, admission: Arc<dyn LocalAdmission>) -> Broker {
        self.engine.local = admission;
        self
    }

    /// Receives every change of jobs and runners from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<BusEvent> {
        self.engine.bus.subscribe()
    }

    /// Serves until the listener closes.
    pub async fn run(self) {
        let Broker {
            mut engine,
            listener,
            tx,
            mut rx,
        } = self;
        let clock = engine.clock.clone();
        let accept = async move {
            let mut id: ConnId = 0;
            loop {
                match listener.accept().await {
                    Ok(t) => {
                        id += 1;
                        conn::spawn(id, t, tx.clone(), clock.clone());
                    }
                    Err(TransportError::Closed) => break,
                    Err(_) => tokio::task::yield_now().await,
                }
            }
        };
        let work = async move {
            while let Some(cmd) = rx.recv().await {
                engine.handle(cmd).await;
            }
        };
        tokio::select! {
            _ = accept => {}
            _ = work => {}
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Client {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) machine_id: String,
    pub(crate) machine: Option<String>,
    pub(crate) owner: bool,
    pub(crate) endpoint: Endpoint,
}

#[derive(Debug, Clone)]
pub(crate) enum Role {
    Pending,
    Hub(Client),
    Runner(String),
}

pub(crate) struct Conn {
    out: mpsc::UnboundedSender<Out>,
    pub(crate) peer: Peer,
    pub(crate) role: Role,
    pub(crate) subs: BTreeMap<u64, Vec<Topic>>,
}

#[derive(Debug, Clone)]
pub(crate) struct Cancel {
    pub(crate) by: String,
    pub(crate) reason: String,
    pub(crate) code: lr_proto::ReasonCode,
    pub(crate) detail: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Job {
    pub(crate) key: JobKey,
    pub(crate) internal: JobId,
    pub(crate) entry: QueueEntry,
    pub(crate) spec: OrderSpec,
    pub(crate) runner: Option<String>,
    pub(crate) assigned_at: Timestamp,
    pub(crate) started_at: Option<Timestamp>,
    pub(crate) cancel: Option<Cancel>,
}

pub(crate) struct Engine {
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) store: Arc<dyn BrokerStore>,
    pub(crate) policy: ActivePolicy,
    pub(crate) site: Site,
    pub(crate) local: Arc<dyn LocalAdmission>,
    pub(crate) bus: broadcast::Sender<BusEvent>,
    pub(crate) conns: HashMap<ConnId, Conn>,
    pub(crate) hubs: HashMap<String, ConnId>,
    pub(crate) runners: BTreeMap<String, RunnerSlot>,
    pub(crate) queue: Vec<Job>,
    pub(crate) active: Vec<Job>,
    pub(crate) served: ServeLog,
    pub(crate) waits: HashMap<JobKey, String>,
    next_id: u64,
    next_sub: u64,
}

const MIB: u64 = 1024 * 1024;

impl Engine {
    async fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Opened { conn, peer, out } => {
                self.conns.insert(
                    conn,
                    Conn {
                        out,
                        peer,
                        role: Role::Pending,
                        subs: BTreeMap::new(),
                    },
                );
            }
            Command::Frame { conn, frame } => {
                let Some(role) = self.conns.get(&conn).map(|c| c.role.clone()) else {
                    return;
                };
                match role {
                    Role::Pending => self.hello(conn, frame).await,
                    Role::Hub(client) => self.on_hub(conn, client, frame).await,
                    Role::Runner(name) => self.on_runner(conn, name, frame).await,
                }
            }
            Command::Closed { conn } => self.closed(conn).await,
        }
    }

    async fn closed(&mut self, conn: ConnId) {
        let Some(c) = self.conns.remove(&conn) else {
            return;
        };
        match c.role {
            Role::Hub(client) => {
                if self.hubs.get(&client.key) == Some(&conn) {
                    self.hubs.remove(&client.key);
                }
            }
            Role::Runner(name) => {
                if self.runners.get(&name).and_then(|r| r.conn) == Some(conn) {
                    self.runner_gone(&name).await;
                }
            }
            Role::Pending => {}
        }
    }

    pub(crate) fn now(&self) -> Timestamp {
        self.clock.now()
    }

    pub(crate) fn internal_id(&mut self) -> JobId {
        self.next_id += 1;
        JobId(format!("#{}", self.next_id))
    }

    pub(crate) fn sub_id(&mut self) -> u64 {
        self.next_sub += 1;
        self.next_sub
    }

    pub(crate) fn send<M: Serialize>(&self, conn: ConnId, msg: &M) {
        if let Ok(frame) = Frame::json(msg) {
            self.send_frame(conn, frame);
        }
    }

    pub(crate) fn send_frame(&self, conn: ConnId, frame: Frame) {
        if let Some(c) = self.conns.get(&conn) {
            let _ = c.out.send(Out::Frame(frame));
        }
    }

    pub(crate) fn close(&self, conn: ConnId) {
        if let Some(c) = self.conns.get(&conn) {
            let _ = c.out.send(Out::Close);
        }
    }

    pub(crate) fn error(&self, conn: ConnId, code: ProtocolErrorCode, message: String) {
        self.send(
            conn,
            &lr_proto::BrokerToHub::Error(ProtocolError {
                code,
                message,
                supported: None,
            }),
        );
    }

    pub(crate) fn refuse(&self, conn: ConnId, code: ProtocolErrorCode, message: String) {
        self.error(conn, code, message);
        self.close(conn);
    }

    pub(crate) fn outbox_limit(&self) -> u64 {
        self.policy
            .file()
            .limits
            .outbox_max_mb
            .get()
            .saturating_mul(MIB)
    }

    pub(crate) fn log_limit(&self) -> u64 {
        self.policy
            .file()
            .limits
            .job_log_max_mb
            .get()
            .saturating_mul(MIB)
    }

    pub(crate) fn lost_kill_s(&self) -> u64 {
        self.policy.file().grace.lost_min.get().saturating_mul(60)
    }

    pub(crate) fn find(&self, key: &JobKey) -> Option<&Job> {
        self.queue
            .iter()
            .chain(self.active.iter())
            .find(|j| &j.key == key)
    }

    pub(crate) fn public_id(&self, internal: &JobId) -> JobId {
        self.queue
            .iter()
            .chain(self.active.iter())
            .find(|j| &j.internal == internal)
            .map_or_else(|| internal.clone(), |j| j.key.job.clone())
    }
}
