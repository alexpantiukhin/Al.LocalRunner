//! The broker engine (spec 13.2): one owner of the queue, the runner registry and the job
//! states, fed by a bounded command channel from the connection tasks; changes go out on a
//! broadcast bus for the read API.

mod admin;
mod allow;
mod assign;
mod cancel;
mod conn;
mod drain;
mod events;
mod follow;
mod hello;
mod inspect;
mod journal;
mod local;
mod lost;
mod metrics;
mod orders;
mod rate;
mod read;
mod recovery;
mod retention;
mod retry;
mod revoke;
mod runners;
mod same_machine;
mod service_budget;
mod service_intake;
mod services;
mod start;
mod wire;

pub use admin::BrokerHandle;
pub use events::BusEvent;
pub use local::{ExactLocalUsers, LocalAdmission, LocalClient};
pub use retention::RETENTION_EVERY_S;

use crate::policy::{ActivePolicy, ListenValue};
use conn::{Command, ConnId, Outlet, COMMAND_QUEUE};
use lr_abstractions::{
    BrokerStore, Clock, JobKey, Level, Listener, LogSink, Meter, Peer, Record, SecretSource,
    TransportError,
};
use lr_core::{Endpoint, JobId, QueueEntry, ServeLog, Site, Timestamp};
use lr_proto::{OrderSpec, Topic};
use runners::RunnerSlot;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

/// Frames waiting to be written to one connection; a peer that lets more pile up is too slow
/// and is disconnected.
pub const OUT_QUEUE_FRAMES: usize = 1024;

/// Event subscriptions one connection may hold at once.
pub const MAX_SUBSCRIPTIONS: usize = 16;

pub(crate) const REPLAY_WINDOW: usize = 256;
pub(crate) const BACKLOG_MAX_FRAMES: usize = 100_000;

const TICK: Duration = Duration::from_secs(1);
const ACCEPT_ERRORS_BEFORE_BACKOFF: u32 = 3;
const ACCEPT_BACKOFF_MAX_MS: u64 = 5_000;

struct NoLog;

impl LogSink for NoLog {
    fn write(&self, _: &Record) {}

    fn dropped(&self) -> u64 {
        0
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
    /// The policy applied at start, from [`ActivePolicy::start`].
    pub policy: ActivePolicy,
    /// Metrics of spec 12.2.
    pub meter: Arc<dyn Meter>,
}

/// The broker role: accepts connections, queues orders and gives them to runners.
pub struct Broker {
    engine: Engine,
    listener: Arc<dyn Listener>,
    tx: mpsc::Sender<Command>,
    rx: mpsc::Receiver<Command>,
}

impl Broker {
    /// A broker over `deps`, admitting local users named exactly in `[[local_user]]`.
    pub fn new(deps: BrokerDeps) -> Broker {
        let (tx, rx) = mpsc::channel(COMMAND_QUEUE);
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
                log: Arc::new(NoLog),
                meter: deps.meter,
                gauges: metrics::Gauges::default(),
                bus: broadcast::channel(1024).0,
                conns: HashMap::new(),
                hubs: HashMap::new(),
                runners: BTreeMap::new(),
                queue: Vec::new(),
                active: Vec::new(),
                served: ServeLog::default(),
                waits: HashMap::new(),
                held_by: HashMap::new(),
                slow_peers: AtomicU64::new(0),
                next_id: 0,
                next_sub: 0,
                booted_at: Timestamp(0),
                restored: false,
                client_seen: HashMap::new(),
                rate: rate::Rate::default(),
                follows: Vec::new(),
                retries: Vec::new(),
                retry_at: Timestamp(0),
                retry_round: false,
                unsaved_starts: false,
                silenced: HashSet::new(),
                retention: retention::Retention::default(),
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

    /// Sends the broker's diagnostic events (slow peers dropped, storage failures, late
    /// messages) to `sink`; without it they are discarded.
    pub fn with_log_sink(mut self, sink: Arc<dyn LogSink>) -> Broker {
        self.engine.log = sink;
        self
    }

    /// Administrative access for the local admin channel; valid while [Broker::run] runs.
    pub fn handle(&self) -> BrokerHandle {
        BrokerHandle(self.tx.clone())
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
        engine.recover().await;
        engine.report();
        engine.sample_outbox().await;
        let clock = engine.clock.clone();
        let log = engine.log.clone();
        let ticks = tx.clone();
        let tick_clock = clock.clone();
        let ticker = async move {
            loop {
                tick_clock.sleep(TICK).await;
                if ticks.send(Command::Tick).await.is_err() {
                    break;
                }
            }
        };
        let accept = async move {
            let mut id: ConnId = 0;
            let mut failures = 0u32;
            loop {
                match listener.accept().await {
                    Ok(t) => {
                        failures = 0;
                        id += 1;
                        conn::spawn(id, t, tx.clone(), clock.clone()).await;
                    }
                    Err(TransportError::Closed) => break,
                    Err(e) => {
                        failures = failures.saturating_add(1);
                        if failures < ACCEPT_ERRORS_BEFORE_BACKOFF {
                            continue;
                        }
                        let step = (failures - ACCEPT_ERRORS_BEFORE_BACKOFF).min(6);
                        let ms = (100u64 << step).min(ACCEPT_BACKOFF_MAX_MS);
                        log.write(
                            &Record::new(
                                clock.now(),
                                Level::Warn,
                                lr_abstractions::Role::Broker,
                                "engine",
                                "accepting connections fails; retrying later",
                            )
                            .field("error", &e.to_string())
                            .field("retry_ms", &ms.to_string()),
                        );
                        clock.sleep(Duration::from_millis(ms)).await;
                    }
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
            _ = ticker => {}
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
    outlet: Outlet,
    pub(crate) peer: Peer,
    backlog: VecDeque<lr_abstractions::Frame>,
    pub(crate) role: Role,
    pub(crate) subs: BTreeMap<u64, Vec<Topic>>,
}

#[derive(Debug, Clone)]
pub(crate) struct Cancel {
    pub(crate) by: String,
    pub(crate) reason: String,
    pub(crate) code: lr_proto::ReasonCode,
    pub(crate) detail: Option<String>,
    pub(crate) sent: bool,
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
    pub(crate) start_unsaved: bool,
    pub(crate) queued_since: Timestamp,
    pub(crate) platform_label: Option<String>,
}

pub(crate) struct Engine {
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) store: Arc<dyn BrokerStore>,
    pub(crate) policy: ActivePolicy,
    pub(crate) site: Site,
    pub(crate) local: Arc<dyn LocalAdmission>,
    pub(crate) log: Arc<dyn LogSink>,
    pub(crate) meter: Arc<dyn Meter>,
    pub(crate) gauges: metrics::Gauges,
    pub(crate) bus: broadcast::Sender<BusEvent>,
    pub(crate) conns: HashMap<ConnId, Conn>,
    pub(crate) hubs: HashMap<String, ConnId>,
    pub(crate) runners: BTreeMap<String, RunnerSlot>,
    pub(crate) queue: Vec<Job>,
    pub(crate) active: Vec<Job>,
    pub(crate) served: ServeLog,
    pub(crate) waits: HashMap<JobKey, String>,
    pub(crate) held_by: HashMap<JobKey, String>,
    pub(crate) slow_peers: AtomicU64,
    next_id: u64,
    next_sub: u64,
    pub(crate) booted_at: Timestamp,
    pub(crate) restored: bool,
    pub(crate) client_seen: HashMap<String, Timestamp>,
    pub(crate) rate: rate::Rate,
    pub(crate) follows: Vec<follow::Follow>,
    pub(crate) retries: Vec<retry::Retry>,
    pub(crate) retry_at: Timestamp,
    pub(crate) retry_round: bool,
    pub(crate) unsaved_starts: bool,
    pub(crate) silenced: HashSet<JobKey>,
    retention: retention::Retention,
}

const MIB: u64 = 1024 * 1024;

impl Engine {
    async fn handle(&mut self, cmd: Command) {
        self.expire().await;
        self.retain();
        match cmd {
            Command::Tick => {
                self.forget_clients();
                self.forget_idle_rates();
                self.report();
                self.sample_outbox().await;
            }
            Command::Opened { conn, peer, outlet } => {
                self.conns.insert(
                    conn,
                    Conn {
                        outlet,
                        peer,
                        backlog: VecDeque::new(),
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
            Command::Drained { conn } => self.pump(conn),
            Command::Admin(cmd) => self.admin(cmd).await,
        }
    }

    async fn closed(&mut self, conn: ConnId) {
        let Some(c) = self.conns.remove(&conn) else {
            return;
        };
        self.follows.retain(|f| f.conn != conn);
        self.note_closed(conn, &c.role);
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
