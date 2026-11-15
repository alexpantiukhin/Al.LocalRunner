use crate::background::{background, recover};
use crate::cache::{CacheLayout, Caches};
use crate::connect::{give_up, session, End};
use crate::diag::Diag;
use crate::outbox::Outbox;
use crate::service_book::Book;
use crate::services::Services;
use crate::settings::{RunnerSettings, MAX_LOST_KILL_S};
use crate::source::WorkspaceLayout;
use lr_abstractions::{
    CacheKey, Clock, Dialer, DiskSpace, Files, HostLoad, KeepAwake, Level, LogSink, Meter,
    ProcessInspector, ProcessRunner, RunnerStore, SecretSource, Stop, StopHandle, TransportError,
};
use lr_core::{JobId, Timestamp};
use lr_proto::{
    DrainReason, ProtocolError, ProtocolErrorCode, ReasonCode, WireCapabilities, WireServiceStatus,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const CONTROL_QUEUE: usize = 8;
pub(crate) const SWEEP_EVERY: Duration = Duration::from_secs(3600);
pub(crate) const FORGET_AFTER_MS: u64 = 24 * 3_600_000;

pub(crate) fn after(t: Timestamp, d: Duration) -> Timestamp {
    Timestamp(t.0.saturating_add(u64::try_from(d.as_millis()).unwrap_or(u64::MAX)))
}

/// Where the broker this runner joined listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerAddress {
    /// `host:port`, or the local IPC name.
    pub address: String,
    /// Pinned certificate fingerprint from the join code; `None` for local IPC.
    pub fingerprint: Option<String>,
}

/// The settings of a runner as a snapshot that can be replaced while the runner runs: a reader
/// takes an `Arc` and keeps it for as long as it needs one value to stay the same.
pub struct SettingsCell(RwLock<Arc<RunnerSettings>>);

impl SettingsCell {
    /// A cell holding `settings`.
    pub fn new(settings: RunnerSettings) -> SettingsCell {
        SettingsCell(RwLock::new(Arc::new(settings)))
    }

    /// The current snapshot.
    pub fn get(&self) -> Arc<RunnerSettings> {
        self.0.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Replaces the snapshot; readers that hold the old one keep it.
    pub fn swap(&self, next: RunnerSettings) {
        *self.0.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(next);
    }
}

/// Everything a runner agent works with; the assembly passes real implementations, tests the
/// fakes of `lr-abstractions`.
pub struct RunnerDeps {
    /// Time source.
    pub clock: Arc<dyn Clock>,
    /// Opens the connection to the broker.
    pub dialer: Arc<dyn Dialer>,
    /// The broker.
    pub broker: BrokerAddress,
    /// `runner.db`.
    pub store: Arc<dyn RunnerStore>,
    /// Runs jobs and kills process trees.
    pub process: Arc<dyn ProcessRunner>,
    /// Checks processes by pid and start fingerprint (orphan search).
    pub inspector: Arc<dyn ProcessInspector>,
    /// Secret values by name.
    pub secrets: Arc<dyn SecretSource>,
    /// The state directory.
    pub files: Arc<dyn Files>,
    /// Diagnostic events.
    pub log: Arc<dyn LogSink>,
    /// Metrics of spec 12.2.
    pub meter: Arc<dyn Meter>,
    /// Settings of this runner.
    pub settings: SettingsCell,
    /// Free space of volumes (`disk_mb`).
    pub disk: Arc<dyn DiskSpace>,
    /// Keeps the machine awake while a job runs.
    pub awake: Arc<dyn KeepAwake>,
    /// Reads the load of this machine for the heartbeat; `None` sends none.
    pub load: Option<Arc<dyn HostLoad>>,
}

/// How [`RunnerAgent::run`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentExit {
    /// Stopped by [`AgentControl`] after `Bye`.
    Stopped,
    /// The broker refused authorization (key unknown or revoked): the job tree was killed and
    /// the outbox dropped.
    Refused(ProtocolError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Control {
    Drain(DrainReason),
    CancelAndStop,
}

/// Local control of a running agent (`down`, spec 3.9).
#[derive(Clone)]
pub struct AgentControl {
    tx: mpsc::Sender<Control>,
    shared: Arc<Shared>,
}

impl AgentControl {
    /// `down --drain`: take no new orders, finish the current job, then `Bye`; the broker is
    /// told why.
    pub async fn drain(&self, reason: DrainReason) {
        let _ = self.tx.send(Control::Drain(reason)).await;
    }

    /// `down --cancel`: cancel the current job with `runner_stopped`, then `Bye`.
    pub async fn cancel_and_stop(&self) {
        let _ = self.tx.send(Control::CancelAndStop).await;
    }

    /// Replaces the settings of the runner (`reload`): jobs that run keep the settings they
    /// started with, the next job and the capabilities the broker sees follow the new ones. What
    /// identifies the runner and its processes (name, state directory, own process, disk ledger,
    /// services) stays as it is: changing it needs a restart.
    pub fn apply_settings(&self, mut next: RunnerSettings) {
        let old = self.shared.deps.settings.get();
        next.name = old.name.clone();
        next.state_dir = old.state_dir.clone();
        next.process = old.process.clone();
        next.disk_ledger = old.disk_ledger.clone();
        next.services = old.services.clone();
        self.shared.deps.settings.swap(next);
        self.shared.state().caps_dirty = true;
        self.shared.wake.notify_one();
    }

    /// Whether the runner has a welcomed session and, when not, since when it has had none
    /// (its start while it never had one).
    pub fn link(&self) -> (bool, Option<Timestamp>) {
        let since = self.shared.state().lost_since;
        (since.is_none(), since)
    }
}

pub(crate) struct Current {
    pub(crate) job: JobId,
    pub(crate) settings: Arc<RunnerSettings>,
    pub(crate) stop: StopHandle,
    pub(crate) reason: Option<ReasonCode>,
    pub(crate) lost_killed: bool,
}

pub(crate) struct State {
    pub(crate) job: Option<Current>,
    pub(crate) broker_lost_kill_s: Option<u64>,
    pub(crate) draining: bool,
    pub(crate) drain_reason: Option<DrainReason>,
    pub(crate) caps_dirty: bool,
    pub(crate) lost_since: Option<Timestamp>,
    pub(crate) in_use: Vec<CacheKey>,
    pub(crate) next_outbox_sample: Timestamp,
    pub(crate) services: Book,
    pub(crate) services_down: Option<bool>,
}

pub(crate) struct Shared {
    pub(crate) deps: RunnerDeps,
    pub(crate) fixed: Arc<RunnerSettings>,
    pub(crate) diag: Arc<Diag>,
    pub(crate) outbox: Outbox,
    pub(crate) wake: Arc<Notify>,
    pub(crate) status: Notify,
    pub(crate) services_changed: Notify,
    pub(crate) sweep_lock: tokio::sync::Mutex<()>,
    pub(crate) state: Mutex<State>,
}

impl Shared {
    pub(crate) fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn now(&self) -> Timestamp {
        self.deps.clock.now()
    }

    /// The settings of the job that runs, as they were when it started; the current ones when
    /// none runs. Never called while the state lock is held.
    pub(crate) fn job_settings(&self) -> Arc<RunnerSettings> {
        let held = self.state().job.as_ref().map(|c| c.settings.clone());
        held.unwrap_or_else(|| self.deps.settings.get())
    }

    pub(crate) fn marker(&self) -> PathBuf {
        self.fixed.state_dir.join("active-job")
    }

    pub(crate) fn workspace(&self) -> WorkspaceLayout {
        WorkspaceLayout::new(self.fixed.state_dir.clone())
    }

    pub(crate) fn caches(&self) -> Caches<'_> {
        let live = self.deps.settings.get();
        Caches {
            store: self.deps.store.as_ref(),
            files: self.deps.files.as_ref(),
            diag: &self.diag,
            meter: self.deps.meter.as_ref(),
            runner: &self.fixed.name,
            layout: CacheLayout::new(self.fixed.state_dir.clone()),
            cache: live.cache,
            workspace: live.workspace,
        }
    }

    pub(crate) fn services(&self) -> Services<'_> {
        Services {
            store: self.deps.store.as_ref(),
            process: self.deps.process.as_ref(),
            inspector: self.deps.inspector.as_ref(),
            diag: &self.diag,
        }
    }

    pub(crate) async fn capabilities(&self) -> WireCapabilities {
        let used = self.caches().used_mb().await;
        let mut caps = self.deps.settings.get().capabilities(used);
        caps.disk_mb = self.free_disk_mb().await;
        for name in &self.state().services.unavailable {
            caps.services
                .insert(name.clone(), WireServiceStatus::Unavailable);
        }
        caps
    }

    pub(crate) fn set_lost(&self, since: Option<Timestamp>) {
        self.state().lost_since = since;
        self.status.notify_one();
    }

    pub(crate) fn set_broker_lost_kill(&self, seconds: Option<u64>) {
        self.state().broker_lost_kill_s = seconds.map(|s| s.min(MAX_LOST_KILL_S));
        self.status.notify_one();
    }

    pub(crate) async fn claim(&self, keys: Vec<CacheKey>) {
        let _serial = self.sweep_lock.lock().await;
        self.state().in_use = keys;
    }

    pub(crate) async fn sweep(&self) {
        let _serial = self.sweep_lock.lock().await;
        let in_use = self.state().in_use.clone();
        self.caches().sweep(self.now(), &in_use).await;
    }

    pub(crate) fn stop_job(&self, stop: Stop, reason: ReasonCode, text: &str) {
        if let Some(cur) = self.state().job.as_mut() {
            cur.reason.get_or_insert(reason);
            cur.stop.stop_with_reason(stop, text);
        }
    }

    pub(crate) async fn forget_finished(&self) {
        let before = Timestamp(self.now().0.saturating_sub(FORGET_AFTER_MS));
        if let Err(e) = self.deps.store.forget_finished(before).await {
            self.diag.event(
                Level::Error,
                "finished jobs not forgotten",
                &[("error", &e.to_string())],
            );
        }
    }
}

#[derive(Default)]
pub(crate) struct JobSlot(Option<JoinHandle<()>>);

impl JobSlot {
    pub(crate) fn spawn(&mut self, task: impl std::future::Future<Output = ()> + Send + 'static) {
        self.0 = Some(tokio::spawn(task));
    }

    pub(crate) async fn wait(&mut self) {
        if let Some(h) = self.0.take() {
            let _ = h.await;
        }
    }
}

impl Drop for JobSlot {
    fn drop(&mut self) {
        if let Some(h) = &self.0 {
            h.abort();
        }
    }
}

/// A runner: connects to its broker (it has no listening port), runs one job at a time and
/// delivers output and results through its outbox (spec 3.3, 3.9).
pub struct RunnerAgent {
    shared: Arc<Shared>,
    control_tx: mpsc::Sender<Control>,
    control_rx: mpsc::Receiver<Control>,
}

impl RunnerAgent {
    /// An agent over `deps`; nothing happens before [`RunnerAgent::run`].
    pub fn new(deps: RunnerDeps) -> RunnerAgent {
        let fixed = deps.settings.get();
        let diag = Arc::new(Diag {
            log: deps.log.clone(),
            clock: deps.clock.clone(),
        });
        let wake = Arc::new(Notify::new());
        let outbox = Outbox {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            diag: diag.clone(),
            wake: wake.clone(),
            tail: Default::default(),
        };
        let (control_tx, control_rx) = mpsc::channel(CONTROL_QUEUE);
        let lost_since = Some(deps.clock.now());
        RunnerAgent {
            shared: Arc::new(Shared {
                deps,
                fixed,
                diag,
                outbox,
                wake,
                status: Notify::new(),
                services_changed: Notify::new(),
                sweep_lock: tokio::sync::Mutex::new(()),
                state: Mutex::new(State {
                    job: None,
                    broker_lost_kill_s: None,
                    draining: false,
                    drain_reason: None,
                    caps_dirty: false,
                    lost_since,
                    in_use: Vec::new(),
                    next_outbox_sample: Timestamp(0),
                    services: Book::default(),
                    services_down: None,
                }),
            }),
            control_tx,
            control_rx,
        }
    }

    /// The local control handle.
    pub fn control(&self) -> AgentControl {
        AgentControl {
            tx: self.control_tx.clone(),
            shared: self.shared.clone(),
        }
    }

    /// Recovers from the previous run, then keeps a session with the broker, reconnecting with
    /// a backoff from 1 to 30 seconds, until stopped or refused. While the broker is
    /// unreachable the running job is killed once the grace has passed since the session was
    /// lost, however long a dial or a welcome takes.
    pub async fn run(mut self) -> AgentExit {
        let shared = self.shared.clone();
        shared.diag.event(Level::Info, "runner started", &[]);
        recover(&shared).await;
        let mut slot = JobSlot::default();
        let mut backoff = MIN_BACKOFF;
        let mut next_sweep = after(shared.now(), SWEEP_EVERY);
        loop {
            let end = tokio::select! {
                end = connect_once(&shared, &mut slot, &mut self.control_rx) => end,
                never = background(&shared, &mut next_sweep) => never,
            };
            match end {
                Some(End::Refused(e)) => return AgentExit::Refused(e),
                Some(End::Stopped) => return AgentExit::Stopped,
                Some(End::Lost { welcomed: true }) => {
                    backoff = MIN_BACKOFF;
                    shared.set_lost(Some(shared.now()));
                }
                Some(End::Lost { welcomed: false }) | None => {}
            }
            let stop = tokio::select! {
                stop = self.wait_offline(&shared, backoff) => stop,
                never = background::<bool>(&shared, &mut next_sweep) => never,
            };
            if stop {
                shared.stop_all_services().await;
                return AgentExit::Stopped;
            }
            backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
        }
    }

    /// Waits for the next dial; `true` when the runner was told to stop while the broker is
    /// unreachable and has nothing left to deliver, so it exits instead of waiting for the broker.
    async fn wait_offline(&mut self, shared: &Shared, backoff: Duration) -> bool {
        let retry_at = after(shared.now(), backoff);
        loop {
            tokio::select! {
                _ = shared.deps.clock.sleep_until(retry_at) => return false,
                cmd = self.control_rx.recv() => {
                    if let Some(cmd) = cmd {
                        apply_control(shared, cmd);
                        if crate::connect::idle_and_stopping(shared).await {
                            return true;
                        }
                    }
                }
            }
        }
    }
}

async fn connect_once(
    shared: &Arc<Shared>,
    slot: &mut JobSlot,
    control: &mut mpsc::Receiver<Control>,
) -> Option<End> {
    let b = &shared.deps.broker;
    match shared
        .deps
        .dialer
        .dial(&b.address, b.fingerprint.as_deref())
        .await
    {
        Ok(t) => Some(session(shared, Arc::from(t), slot, control).await),
        Err(e) => {
            if let Some(refusal) = handshake_refusal(&e) {
                give_up(shared, slot, &refusal).await;
                return Some(End::Refused(refusal));
            }
            shared.diag.event(
                Level::Warn,
                "broker unreachable",
                &[("address", &b.address), ("error", &e.to_string())],
            );
            None
        }
    }
}

fn handshake_refusal(e: &TransportError) -> Option<ProtocolError> {
    let TransportError::Refused(text) = e else {
        return None;
    };
    let (code, message) = text.split_once(':')?;
    let code: ProtocolErrorCode =
        serde_json::from_value(serde_json::Value::String(code.trim().to_string())).ok()?;
    code.kills_running_jobs().then(|| ProtocolError {
        code,
        message: message.trim().to_string(),
        supported: None,
    })
}

pub(crate) fn apply_control(shared: &Shared, cmd: Control) {
    let reason = match cmd {
        Control::Drain(reason) => reason,
        Control::CancelAndStop => DrainReason::Down,
    };
    {
        let mut st = shared.state();
        st.draining = true;
        st.drain_reason = Some(reason);
    }
    shared.diag.event(
        Level::Info,
        "drain requested",
        &[("reason", &format!("{reason:?}").to_lowercase())],
    );
    if cmd == Control::CancelAndStop {
        shared.stop_job(Stop::Cancel, ReasonCode::RunnerStopped, "runner stopped");
    }
    shared.wake.notify_one();
}
