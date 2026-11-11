use crate::client::{self, Pending, Target};
use crate::limits::{Overflow, Overflows};
use crate::orders::JobIds;
use crate::policy::HubPolicy;
use crate::reaper::{Reaper, Watched};
use crate::select::Decision;
use crate::track::{Finished, Link, Stage, Track};
use lr_abstractions::{
    Clock, Dialer, Files, Frame, HubStore, Level, Listener, LogSink, Meter, OutboxKind,
    ProcessInspector, Record, Repository, Role,
};
use lr_core::{JobId, Timestamp, Zone};
use lr_proto::{
    BrokerToHub, CliReply, CliRequest, HubToBroker, OrderSpec, ProtocolError, ProtocolErrorCode,
    ReasonCode, RejectReason,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Notify};

/// Name under which the broker of this machine (reached over local IPC) is listed.
pub const LOCAL_BROKER: &str = "local";

/// Rereads the diagnostics configuration of the hub (`logging reload`); `Err` says why the
/// new configuration was refused.
pub type ReloadLogging = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

const EVENT_QUEUE: usize = 1024;
const TICK: Duration = Duration::from_secs(1);

/// What the hub is built from; only `lr-cli` knows the implementations behind them.
pub struct HubDeps {
    /// Time source.
    pub clock: Arc<dyn Clock>,
    /// `hub.db` and the log copies.
    pub store: Arc<dyn HubStore>,
    /// Local IPC of the CLI.
    pub ipc: Arc<dyn Listener>,
    /// Connections to brokers: TLS to the joined ones, local IPC to the local one.
    pub dialer: Arc<dyn Dialer>,
    /// IPC address of the broker of this machine, listed as [`LOCAL_BROKER`].
    pub local_broker: Option<String>,
    /// Liveness of submitter processes (reaper).
    pub inspector: Arc<dyn ProcessInspector>,
    /// Profile files of work trees and job handles.
    pub files: Arc<dyn Files>,
    /// Commits, push state and committed profiles.
    pub repository: Arc<dyn Repository>,
    /// Diagnostic events.
    pub log: Arc<dyn LogSink>,
    /// Metrics of spec 12.2.
    pub meter: Arc<dyn Meter>,
    /// `hub-policy.toml`.
    pub policy: HubPolicy,
    /// Name of this hub in `hello`.
    pub name: String,
}

pub(crate) struct Shared {
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) store: Arc<dyn HubStore>,
    pub(crate) dialer: Arc<dyn Dialer>,
    pub(crate) inspector: Arc<dyn ProcessInspector>,
    pub(crate) files: Arc<dyn Files>,
    pub(crate) repository: Arc<dyn Repository>,
    pub(crate) log: Arc<dyn LogSink>,
    pub(crate) meter: Arc<dyn Meter>,
    pub(crate) name: String,
    pub(crate) reload_logging: Option<ReloadLogging>,
    pub(crate) pending: Pending,
    next_req: AtomicU64,
    pub(crate) overflows: Overflows,
}

impl Shared {
    pub(crate) fn next_req(&self) -> u64 {
        self.next_req
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    }

    pub(crate) fn diag(&self, level: Level, message: &str, fields: &[(&str, &str)]) {
        let mut r = Record::new(self.clock.now(), level, Role::Hub, "hub", message);
        for (k, v) in fields {
            r = r.field(k, v);
        }
        self.log.write(&r);
    }
}

pub(crate) fn refusal(code: ProtocolErrorCode, message: impl Into<String>) -> ProtocolError {
    ProtocolError {
        code,
        message: message.into(),
        supported: None,
    }
}

pub(crate) type ConnId = u64;

pub(crate) struct CliEvent {
    pub(crate) conn: ConnId,
    pub(crate) os_user: String,
    pub(crate) req: CliRequest,
    pub(crate) reply: mpsc::Sender<CliReply>,
    pub(crate) hangup: Arc<Notify>,
}

pub(crate) enum BrokerEvent {
    Connected {
        zone: Option<Zone>,
        out: mpsc::Sender<Frame>,
        kill: Arc<Notify>,
    },
    Disconnected,
    Refused(ProtocolError),
    Message(Box<BrokerToHub>),
}

pub(crate) enum Event {
    Cli(Box<CliEvent>),
    CliClosed(ConnId),
    Broker(String, BrokerEvent),
    Selected {
        job: JobId,
        gen: u64,
        decision: Decision,
    },
    Tick,
    Shutdown,
}

pub(crate) struct Engine {
    pub(crate) s: Arc<Shared>,
    pub(crate) policy: HubPolicy,
    pub(crate) links: BTreeMap<String, Link>,
    pub(crate) tracks: BTreeMap<JobId, Track>,
    pub(crate) recent: VecDeque<(JobId, Finished)>,
    pub(crate) submitted: BTreeMap<ConnId, BTreeSet<JobId>>,
    pub(crate) ids: JobIds,
    pub(crate) reaper: Reaper,
    pub(crate) tx: mpsc::Sender<Event>,
    pub(crate) next_outbox_sample: Timestamp,
}

/// The hub role.
pub struct Hub {
    deps: HubDeps,
    reload_logging: Option<ReloadLogging>,
}

impl Hub {
    /// A hub over `deps`.
    pub fn new(deps: HubDeps) -> Hub {
        Hub {
            deps,
            reload_logging: None,
        }
    }

    /// The hub answers `logging reload` on its CLI channel with `reload`; without it the
    /// command is refused.
    pub fn with_logging_reload(mut self, reload: ReloadLogging) -> Hub {
        self.reload_logging = Some(reload);
        self
    }

    /// Serves until the IPC listener closes or its owner asks it to stop (`down`).
    pub async fn run(self) {
        let d = self.deps;
        let reload_logging = self.reload_logging;
        let shared = Arc::new(Shared {
            reload_logging,
            clock: d.clock.clone(),
            store: d.store,
            dialer: d.dialer,
            inspector: d.inspector,
            files: d.files,
            repository: d.repository,
            log: d.log,
            meter: d.meter,
            name: d.name,
            pending: Pending::default(),
            next_req: AtomicU64::new(0),
            overflows: Overflows::default(),
        });
        let (tx, mut rx) = mpsc::channel(EVENT_QUEUE);
        let mut engine = Engine {
            s: shared.clone(),
            policy: d.policy,
            links: BTreeMap::new(),
            tracks: BTreeMap::new(),
            recent: VecDeque::new(),
            submitted: BTreeMap::new(),
            ids: JobIds::default(),
            reaper: Reaper::new(d.clock.now()),
            tx: tx.clone(),
            next_outbox_sample: d.clock.now(),
        };
        let targets = engine.targets(d.local_broker).await;
        engine.restore().await;
        engine.sample_outbox().await;
        for t in targets {
            engine.links.entry(t.name.clone()).or_default();
            client::spawn(t, shared.clone(), tx.clone());
        }
        let (ipc_tx, ipc_shared) = (tx.clone(), shared.clone());
        let ipc =
            tokio::spawn(async move { crate::ipc_server::serve(d.ipc, ipc_shared, ipc_tx).await });
        let tick_tx = tx.clone();
        let clock = d.clock;
        let ticks = tokio::spawn(async move {
            loop {
                clock.sleep(TICK).await;
                if tick_tx.send(Event::Tick).await.is_err() {
                    return;
                }
            }
        });
        engine.reap().await;
        let waiting: Vec<JobId> = engine
            .tracks
            .iter()
            .filter(|(_, t)| t.stage == Stage::Selecting && t.spec.is_some())
            .map(|(j, _)| j.clone())
            .collect();
        for job in waiting {
            engine.start_selection(&job);
        }
        while let Some(ev) = rx.recv().await {
            if !engine.handle(ev).await {
                break;
            }
        }
        ipc.abort();
        ticks.abort();
    }
}

fn spec_of(payload: &str) -> Option<OrderSpec> {
    match serde_json::from_str::<HubToBroker>(payload).ok()? {
        HubToBroker::Order(o) => Some(*o.spec),
        _ => None,
    }
}

impl Engine {
    pub(crate) fn now(&self) -> Timestamp {
        self.s.clock.now()
    }

    pub(crate) fn store_failed(&self, what: &str, job: &JobId, e: &lr_abstractions::StoreError) {
        self.s.diag(
            Level::Error,
            what,
            &[("job", &job.0), ("error", &e.to_string())],
        );
    }

    async fn targets(&mut self, local: Option<String>) -> Vec<Target> {
        let mut targets = Vec::new();
        if let Some(address) = local {
            targets.push(Target {
                name: LOCAL_BROKER.into(),
                address,
                fingerprint: None,
            });
        }
        match self.s.store.brokers().await {
            Ok(list) => {
                for b in list.into_iter().filter(|b| b.name != LOCAL_BROKER) {
                    self.links.insert(
                        b.name.clone(),
                        Link {
                            zone: b.last_zone,
                            ..Link::default()
                        },
                    );
                    targets.push(Target {
                        name: b.name,
                        address: b.address,
                        fingerprint: Some(b.fingerprint),
                    });
                }
            }
            Err(e) => self.s.diag(
                Level::Error,
                "broker list unreadable",
                &[("error", &e.to_string())],
            ),
        }
        targets
    }

    async fn restore(&mut self) {
        let orders = match self.s.store.active_orders().await {
            Ok(o) => o,
            Err(e) => {
                self.s.diag(
                    Level::Error,
                    "active orders unreadable",
                    &[("error", &e.to_string())],
                );
                return;
            }
        };
        let pending = match self.s.store.pending_outbox().await {
            Ok(p) => p,
            Err(e) => {
                self.s.diag(
                    Level::Error,
                    "outbox unreadable",
                    &[("error", &e.to_string())],
                );
                Vec::new()
            }
        };
        let floor = match self.s.store.job_id_floor().await {
            Ok(f) => f,
            Err(e) => {
                self.s.diag(
                    Level::Error,
                    "job-id floor unreadable",
                    &[("error", &e.to_string())],
                );
                0
            }
        };
        let restored = orders.iter().filter_map(|o| JobIds::time_of(&o.job)).max();
        self.ids = JobIds::above(floor.max(restored.unwrap_or(0)));
        let mut broken = Vec::new();
        for o in orders {
            let mine: Vec<_> = pending.iter().filter(|e| e.job == o.job).collect();
            let entry = mine.iter().find(|e| e.kind == OutboxKind::Order);
            let stored = o
                .spec_json
                .as_deref()
                .and_then(|j| serde_json::from_str::<OrderSpec>(j).ok());
            let spec = stored.or_else(|| entry.and_then(|e| spec_of(&e.payload)));
            let unreadable = mine
                .iter()
                .any(|e| serde_json::from_str::<HubToBroker>(&e.payload).is_err());
            let stage = match (&o.broker, entry) {
                (None, _) => Stage::Selecting,
                (Some(_), Some(_)) => Stage::Sent,
                (Some(_), None) => Stage::Accepted,
            };
            if unreadable || (o.broker.is_none() && spec.is_none()) {
                broken.push(o.job.clone());
            }
            let mut track = Track::new(o, spec, stage);
            track.cancelled = mine.iter().any(|e| e.kind == OutboxKind::Cancel);
            self.tracks.insert(track.order.job.clone(), track);
        }
        for job in broken {
            self.unreadable(&job).await;
        }
    }

    pub(crate) async fn unreadable(&mut self, job: &JobId) {
        self.s.diag(
            Level::Error,
            "order or outbox entry in hub.db unreadable: the job is ended",
            &[("job", &job.0)],
        );
        let code = ReasonCode::Rejected(RejectReason::Other("rejected_order_unreadable".into()));
        let detail = "запись заказа в hub.db не читается".to_string();
        self.final_reject(job, code, detail).await;
    }

    async fn handle(&mut self, ev: Event) -> bool {
        match ev {
            Event::Cli(e) if e.req == CliRequest::Shutdown => {
                let _ = e.reply.try_send(CliReply::Stopping);
                return false;
            }
            Event::Cli(e) => self.on_cli(*e).await,
            Event::CliClosed(conn) => {
                self.submitted.remove(&conn);
                for t in self.tracks.values_mut() {
                    t.watchers.retain(|(c, _, _)| *c != conn);
                }
            }
            Event::Broker(name, ev) => self.on_broker(&name, ev).await,
            Event::Selected { job, gen, decision } => self.on_decision(&job, gen, decision).await,
            Event::Tick => self.on_tick().await,
            Event::Shutdown => return false,
        }
        true
    }

    async fn on_tick(&mut self) {
        self.reap().await;
        self.expire().await;
        self.sample_outbox().await;
        let now = self.now();
        let due: Vec<JobId> = self
            .tracks
            .iter()
            .filter(|(_, t)| {
                t.stage == Stage::Waiting
                    && t.spec.is_some()
                    && t.next_select.is_some_and(|at| at <= now)
            })
            .map(|(j, _)| j.clone())
            .collect();
        for job in due {
            self.start_selection(&job);
        }
        let names: Vec<String> = self.links.keys().cloned().collect();
        for name in names {
            let Some(link) = self.links.get_mut(&name) else {
                continue;
            };
            if link.resend_at.is_some_and(|at| at <= now) {
                link.resend_at = None;
                link.sent.clear();
                link.flush_later = true;
            }
            if link.flush_later {
                self.flush(&name).await;
            }
        }
    }

    async fn reap(&mut self) {
        let watched: Vec<Watched> = self
            .tracks
            .values()
            .filter(|t| !t.order.detached && !t.cancelled)
            .map(|t| Watched {
                job: t.order.job.clone(),
                pid: t.order.submitter_pid,
                start: t.order.submitter_start,
            })
            .collect();
        let now = self.now();
        let sweep = self.reaper.tick(now, &watched, self.s.inspector.as_ref());
        for job in &sweep.unknown {
            self.s.diag(
                Level::Warn,
                "submitter start time unknown: its liveness cannot be checked and the job is kept",
                &[("job", &job.0)],
            );
        }
        for job in sweep.dead {
            self.s.diag(
                Level::Info,
                "submitter is gone: its job is cancelled",
                &[("job", &job.0)],
            );
            let by = match self.tracks.get(&job) {
                Some(t) => t.owner(),
                None => continue,
            };
            let cancel = crate::cli::CancelBy {
                by: &by,
                name: "hub",
                force: false,
                code: ReasonCode::SenderGone,
                reason: Some("отправитель завершился"),
            };
            self.cancel_job(&job, cancel).await;
        }
    }

    pub(crate) fn notify(&mut self, job: &JobId, reply: &CliReply) {
        let Some(track) = self.tracks.get_mut(job) else {
            return;
        };
        let before = track.watchers.len();
        track.watchers.retain(|(_, tx, hangup)| {
            let kept = tx.try_send(reply.clone()).is_ok();
            if !kept {
                hangup.notify_one();
            }
            kept
        });
        if track.watchers.len() < before {
            self.s.overflow(
                Overflow::Watcher,
                Level::Warn,
                "slow or closed watcher dropped; the log copy holds the full output",
                &[("job", &job.0)],
            );
        }
    }

    pub(crate) async fn log_copy(&self, job: &JobId, text: &str) {
        let Some(path) = self.tracks.get(job).and_then(|t| t.log.clone()) else {
            return;
        };
        if let Err(e) = self.s.store.append_log_copy(&path, text.as_bytes()).await {
            self.store_failed("log copy not written", job, &e);
        }
    }
}
