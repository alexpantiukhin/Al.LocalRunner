use crate::cache::{CacheLayout, Caches};
use crate::connect::{session, End};
use crate::diag::Diag;
use crate::outbox::Outbox;
use crate::services::Services;
use crate::settings::RunnerSettings;
use crate::source::WorkspaceLayout;
use lr_abstractions::{
    Clock, Dialer, Files, Level, LogSink, OutboxKind, ProcessInspector, ProcessRunner, RunnerStore,
    SecretSource, Stop, StopHandle,
};
use lr_core::{JobId, JobState, Timestamp};
use lr_proto::{JobResult, ProtocolError, ReasonCode, RunnerToBroker, WireCapabilities};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const CONTROL_QUEUE: usize = 8;
const SWEEP_EVERY: Duration = Duration::from_secs(3600);
pub(crate) const FORGET_AFTER_MS: u64 = 24 * 3_600_000;

/// Where the broker this runner joined listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerAddress {
    /// `host:port`, or the local IPC name.
    pub address: String,
    /// Pinned certificate fingerprint from the join code; `None` for local IPC.
    pub fingerprint: Option<String>,
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
    /// Settings of this runner.
    pub settings: RunnerSettings,
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
    Drain,
    CancelAndStop,
}

/// Local control of a running agent (`down`, spec 3.9).
#[derive(Clone)]
pub struct AgentControl(mpsc::Sender<Control>);

impl AgentControl {
    /// `down --drain`: take no new orders, finish the current job, then `Bye`.
    pub async fn drain(&self) {
        let _ = self.0.send(Control::Drain).await;
    }

    /// `down --cancel`: cancel the current job with `runner_stopped`, then `Bye`.
    pub async fn cancel_and_stop(&self) {
        let _ = self.0.send(Control::CancelAndStop).await;
    }
}

pub(crate) struct Current {
    pub(crate) job: JobId,
    pub(crate) stop: StopHandle,
    pub(crate) reason: Option<ReasonCode>,
    pub(crate) lost_killed: bool,
}

pub(crate) struct State {
    pub(crate) job: Option<Current>,
    pub(crate) broker_lost_kill_s: Option<u64>,
    pub(crate) draining: bool,
    pub(crate) caps_dirty: bool,
}

pub(crate) struct Shared {
    pub(crate) deps: RunnerDeps,
    pub(crate) diag: Arc<Diag>,
    pub(crate) outbox: Outbox,
    pub(crate) wake: Arc<Notify>,
    pub(crate) state: Mutex<State>,
}

impl Shared {
    pub(crate) fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn now(&self) -> Timestamp {
        self.deps.clock.now()
    }

    pub(crate) fn marker(&self) -> PathBuf {
        self.deps.settings.state_dir.join("active-job")
    }

    pub(crate) fn workspace(&self) -> WorkspaceLayout {
        WorkspaceLayout::new(self.deps.settings.state_dir.clone())
    }

    pub(crate) fn caches(&self) -> Caches<'_> {
        Caches {
            store: self.deps.store.as_ref(),
            files: self.deps.files.as_ref(),
            diag: &self.diag,
            layout: CacheLayout::new(self.deps.settings.state_dir.clone()),
            cache: self.deps.settings.cache,
            workspace: self.deps.settings.workspace,
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
        self.deps.settings.capabilities(used)
    }

    pub(crate) fn lost_kill(&self) -> Duration {
        let broker = self.state().broker_lost_kill_s;
        Duration::from_secs(self.deps.settings.lost_kill_s(broker))
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
        };
        let (control_tx, control_rx) = mpsc::channel(CONTROL_QUEUE);
        RunnerAgent {
            shared: Arc::new(Shared {
                deps,
                diag,
                outbox,
                wake,
                state: Mutex::new(State {
                    job: None,
                    broker_lost_kill_s: None,
                    draining: false,
                    caps_dirty: false,
                }),
            }),
            control_tx,
            control_rx,
        }
    }

    /// The local control handle.
    pub fn control(&self) -> AgentControl {
        AgentControl(self.control_tx.clone())
    }

    /// Recovers from the previous run, then keeps a session with the broker, reconnecting with
    /// a backoff from 1 to 30 seconds, until stopped or refused.
    pub async fn run(mut self) -> AgentExit {
        let shared = self.shared.clone();
        recover(&shared).await;
        let mut slot = JobSlot::default();
        let mut backoff = MIN_BACKOFF;
        let mut lost_since = shared.now();
        let mut next_sweep = Timestamp(shared.now().0 + SWEEP_EVERY.as_millis() as u64);
        loop {
            let b = &shared.deps.broker;
            match shared
                .deps
                .dialer
                .dial(&b.address, b.fingerprint.as_deref())
                .await
            {
                Ok(t) => match tokio::select! {
                    end = session(&shared, t.as_ref(), &mut slot, &mut self.control_rx) => end,
                    end = sweeper(&shared, &mut next_sweep) => end,
                } {
                    End::Refused(e) => return AgentExit::Refused(e),
                    End::Stopped => return AgentExit::Stopped,
                    End::Lost { welcomed: true } => {
                        backoff = MIN_BACKOFF;
                        lost_since = shared.now();
                    }
                    End::Lost { welcomed: false } => {}
                },
                Err(e) => shared.diag.event(
                    Level::Warn,
                    "broker unreachable",
                    &[("address", &b.address), ("error", &e.to_string())],
                ),
            }
            tokio::select! {
                _ = self.wait_offline(&shared, backoff, lost_since) => {}
                _ = sweeper::<()>(&shared, &mut next_sweep) => {}
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    async fn wait_offline(&mut self, shared: &Shared, backoff: Duration, lost_since: Timestamp) {
        let clock = shared.deps.clock.clone();
        let retry_at = Timestamp(shared.now().0 + backoff.as_millis() as u64);
        loop {
            let (running, killed) = shared
                .state()
                .job
                .as_ref()
                .map_or((false, false), |c| (true, c.lost_killed));
            let kill_at = Timestamp(lost_since.0 + shared.lost_kill().as_millis() as u64);
            if running && !killed && shared.now() >= kill_at {
                if let Some(c) = shared.state().job.as_mut() {
                    c.lost_killed = true;
                }
                shared.diag.event(
                    Level::Warn,
                    "broker unreachable past the grace, job killed",
                    &[],
                );
                shared.stop_job(
                    Stop::Kill,
                    ReasonCode::BrokerUnavailable,
                    "broker unreachable past the grace",
                );
            }
            if shared.now() >= retry_at {
                return;
            }
            let until = if running && !killed && kill_at > shared.now() {
                kill_at.min(retry_at)
            } else {
                retry_at
            };
            tokio::select! {
                _ = clock.sleep_until(until) => {}
                cmd = self.control_rx.recv() => {
                    if let Some(cmd) = cmd {
                        apply_control(shared, cmd);
                    }
                }
            }
        }
    }
}

async fn sweeper<T>(shared: &Shared, next: &mut Timestamp) -> T {
    loop {
        shared.deps.clock.sleep_until(*next).await;
        next.0 += SWEEP_EVERY.as_millis() as u64;
        if shared.state().job.is_none() {
            shared.caches().sweep(shared.now(), &[]).await;
        }
    }
}

pub(crate) fn apply_control(shared: &Shared, cmd: Control) {
    shared.state().draining = true;
    if cmd == Control::CancelAndStop {
        shared.stop_job(Stop::Cancel, ReasonCode::RunnerStopped, "runner stopped");
    }
    shared.wake.notify_one();
}

async fn recover(shared: &Shared) {
    let marker = shared.marker();
    match shared.deps.files.read(&marker).await {
        Ok(Some(bytes)) => {
            let job = JobId(String::from_utf8_lossy(&bytes).trim().to_string());
            restarted(shared, job).await;
            if let Err(e) = shared.deps.files.remove_file(&marker).await {
                shared.diag.event(
                    Level::Error,
                    "active job marker not removed",
                    &[("error", &e.to_string())],
                );
            }
        }
        Ok(None) => {}
        Err(e) => shared.diag.event(
            Level::Error,
            "active job marker unreadable",
            &[("error", &e.to_string())],
        ),
    }
    shared.services().kill_orphans().await;
    shared.forget_finished().await;
    shared.caches().sweep(shared.now(), &[]).await;
}

async fn restarted(shared: &Shared, job: JobId) {
    let pending = match shared.deps.store.pending_outbox().await {
        Ok(p) => p,
        Err(e) => {
            shared.diag.event(
                Level::Error,
                "outbox unreadable at start",
                &[("error", &e.to_string())],
            );
            Vec::new()
        }
    };
    if pending
        .iter()
        .any(|e| e.job == job && e.kind == OutboxKind::JobResult)
    {
        return;
    }
    shared.diag.event(
        Level::Warn,
        "runner restarted while a job ran",
        &[("job", &job.0)],
    );
    let msg = RunnerToBroker::JobResult(JobResult {
        job: job.clone(),
        seq: 0,
        state: JobState::Failed,
        exit_code: None,
        reason: Some(ReasonCode::RunnerRestarted),
        detail: Some("the runner restarted while the job ran".into()),
    });
    shared.outbox.push_until_stored(&job, &msg).await;
}
