use crate::secrets::SecretValue;
use async_trait::async_trait;
use lr_core::{JobId, JobState, StepSpec, StepState, Timestamp};
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::watch;

/// Grace between the soft stop signal of a cancel and killing the tree (spec 7.3).
pub const DEFAULT_CANCEL_GRACE: Duration = Duration::from_secs(10);

/// Replacement for a secret value found in job output (spec 3.13).
pub const MASK: &str = "***";

/// One step of a job as the runner executes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobStep {
    /// The step from the order.
    pub spec: StepSpec,
    /// Secrets this step requested, as environment variable name and value; only this step's
    /// processes see them (spec 3.13).
    pub secrets: Vec<(String, SecretValue)>,
}

impl From<StepSpec> for JobStep {
    fn from(spec: StepSpec) -> JobStep {
        JobStep {
            spec,
            secrets: Vec::new(),
        }
    }
}

/// Everything a [`ProcessRunner`] needs to run one job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpec {
    /// The job.
    pub job: JobId,
    /// Steps in order.
    pub steps: Vec<JobStep>,
    /// Job timeout: no step runs past it.
    pub timeout_s: u64,
    /// Working directory of every step (prepared by the `source` step).
    pub cwd: PathBuf,
    /// The complete environment of the job, built from scratch by the runner: nothing of the
    /// runner process environment is inherited (spec 3.13).
    pub env: Vec<(String, String)>,
    /// Grace between the soft signal and the kill on [`Stop::Cancel`].
    pub cancel_grace: Duration,
    /// Name and value pairs written in the log header of each step (project, source, label,
    /// note, branch, cwd, cancel reason).
    pub header: Vec<(String, String)>,
}

impl JobSpec {
    /// A job in the current directory with an empty environment and the default grace.
    pub fn new(job: JobId, timeout_s: u64, steps: Vec<StepSpec>) -> JobSpec {
        JobSpec {
            job,
            steps: steps.into_iter().map(JobStep::from).collect(),
            timeout_s,
            cwd: PathBuf::from("."),
            env: Vec::new(),
            cancel_grace: DEFAULT_CANCEL_GRACE,
            header: Vec::new(),
        }
    }

    /// Every secret value of every step, for masking.
    pub fn secret_values(&self) -> Vec<&SecretValue> {
        self.steps
            .iter()
            .flat_map(|s| s.secrets.iter().map(|(_, v)| v))
            .collect()
    }
}

/// How one step ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepOutcome {
    /// Step name.
    pub name: String,
    /// Final step state.
    pub state: StepState,
    /// Exit code when the process exited by itself.
    pub exit_code: Option<i32>,
    /// When the step started; `None` when it never started.
    pub started_at: Option<Timestamp>,
    /// When the step ended; `None` when it never started.
    pub finished_at: Option<Timestamp>,
}

/// How a job ended, as the runner reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobOutcome {
    /// `Passed`, `Failed`, `Timeout` or `Cancelled`.
    pub state: JobState,
    /// Exit code of the last step that exited by itself.
    pub exit_code: Option<i32>,
    /// One outcome per step, in order.
    pub steps: Vec<StepOutcome>,
}

/// Progress of a running job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobEvent {
    /// Step `index` started.
    StepStarted {
        /// Step index.
        index: usize,
        /// Step name.
        name: String,
        /// Start time.
        at: Timestamp,
    },
    /// Raw output of step `index`, secrets already masked.
    Output {
        /// Step index.
        index: usize,
        /// Output bytes, not necessarily UTF-8 or line aligned.
        data: Vec<u8>,
    },
    /// Step `index` ended.
    StepFinished {
        /// Step index.
        index: usize,
        /// Its outcome.
        outcome: StepOutcome,
    },
}

/// Receives the output and step boundaries of a job. This is job data (spec 8), not
/// diagnostics: it never goes to a [`crate::LogSink`] (spec 12). A slow receiver
/// back-pressures the job.
#[async_trait]
pub trait JobOutput: Send + Sync {
    /// Takes one event.
    async fn emit(&self, event: JobEvent);
}

/// What stopping a job asks for; `Kill` is stronger than `Cancel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stop {
    /// Soft signal (Ctrl+Break on Windows, SIGTERM elsewhere), then kill the tree after the
    /// job's `cancel_grace` (spec 7.3).
    Cancel,
    /// Kill the tree at once (authorization refused, broker gone past its grace).
    Kill,
}

/// Requests a stop of a running job.
#[derive(Debug)]
pub struct StopHandle(watch::Sender<Option<Stop>>);

/// Watched by a running job for stop requests.
#[derive(Debug, Clone)]
pub struct StopSignal(watch::Receiver<Option<Stop>>);

/// A connected stop handle and signal.
pub fn stop_signal() -> (StopHandle, StopSignal) {
    let (tx, rx) = watch::channel(None);
    (StopHandle(tx), StopSignal(rx))
}

impl StopHandle {
    /// Requests `stop`; a weaker request never replaces a stronger one.
    pub fn stop(&self, stop: Stop) {
        self.0.send_if_modified(|cur| {
            if cur.is_none_or(|c| stop > c) {
                *cur = Some(stop);
                true
            } else {
                false
            }
        });
    }
}

impl StopSignal {
    /// The strongest stop requested so far.
    pub fn current(&self) -> Option<Stop> {
        *self.0.borrow()
    }

    /// Completes once a stop at least as strong as `level` is requested; never completes when
    /// the handle is dropped without one.
    pub async fn reached(&mut self, level: Stop) -> Stop {
        loop {
            if let Some(stop) = *self.0.borrow_and_update() {
                if stop >= level {
                    return stop;
                }
            }
            if self.0.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// A process on this machine, identified by pid and its per-OS start fingerprint, so a reused
/// pid is never mistaken for the original process (spec 7.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProcessRef {
    /// Process id.
    pub pid: u32,
    /// Start fingerprint as the OS reports it; `None` when it could not be obtained.
    pub start: Option<u64>,
}

/// A long-lived process started outside a job: a runner service (spec 3.11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    /// Command line.
    pub command: String,
    /// Working directory.
    pub cwd: PathBuf,
    /// Complete environment.
    pub env: Vec<(String, String)>,
    /// Secrets explicitly requested by the service.
    pub secrets: Vec<(String, SecretValue)>,
}

impl SpawnSpec {
    /// A command with an empty environment.
    pub fn new(command: &str, cwd: PathBuf) -> SpawnSpec {
        SpawnSpec {
            command: command.to_string(),
            cwd,
            env: Vec::new(),
            secrets: Vec::new(),
        }
    }
}

/// Why a process operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessError {
    /// The process could not be started.
    Spawn(String),
    /// The process has no start fingerprint, so killing by pid could hit a reused pid.
    Unverifiable(u32),
    /// Any other failure.
    Io(String),
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProcessError::Spawn(e) => write!(f, "spawn failed: {e}"),
            ProcessError::Unverifiable(pid) => {
                write!(f, "process {pid} has no start time, not killed")
            }
            ProcessError::Io(e) => write!(f, "i/o: {e}"),
        }
    }
}

impl std::error::Error for ProcessError {}

/// Runs jobs and kills process trees (implemented by `lr-exec`).
///
/// Contract of `run_job`: steps run in order in `cwd`; each step is killed at the earlier of
/// its own `timeout_s` and the time left of the job's `timeout_s`, at once and without a soft
/// signal, and ends `timeout`; a step reached with no time left is `timeout` without starting.
/// A failed step skips the rest unless it has `continue_on_failure`; the job is then still
/// `failed`. On [`Stop::Cancel`] the running step gets the soft signal and is killed after
/// `cancel_grace`; on [`Stop::Kill`] it is killed at once; it ends `cancelled`, later steps
/// `skipped`, the job `cancelled`, and `run_job` returns only when the tree is dead. Output
/// is passed to `output` with every secret value of the job replaced by [`MASK`]. A step that
/// cannot be spawned is `failed` without an exit code. All times come from the runner's clock.
#[async_trait]
pub trait ProcessRunner: Send + Sync {
    /// Runs `spec` to its end and reports how it ended.
    async fn run_job(&self, spec: JobSpec, output: &dyn JobOutput, stop: StopSignal) -> JobOutcome;

    /// Starts a long-lived process inside its own process container (Job Object, cgroup or
    /// process group) and returns it with its start fingerprint.
    async fn spawn(&self, spec: SpawnSpec) -> Result<ProcessRef, ProcessError>;

    /// Kills the whole tree of `process` if it is still the same process (same start
    /// fingerprint); a process already gone is not an error. Refuses a reference without a
    /// fingerprint.
    async fn kill_tree(&self, process: &ProcessRef) -> Result<(), ProcessError>;
}

/// Whether a process recorded earlier still runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// A process with this pid and this start fingerprint runs.
    Alive,
    /// No process with this pid, or its start fingerprint differs (the pid was reused).
    Dead,
    /// The start fingerprint could not be read, or the pid is 0: never treated as dead; the
    /// caller logs a warning instead of killing (spec 7.3).
    Unknown,
}

/// Checks processes of this machine by pid and start fingerprint (hub reaper, runner orphan
/// search).
pub trait ProcessInspector: Send + Sync {
    /// Liveness of the process recorded as `pid` started at `start`.
    fn liveness(&self, pid: u32, start: u64) -> Liveness;
}

/// Replaces every occurrence of each non-empty secret value in `data` with [`MASK`].
pub fn mask_secrets(data: &[u8], secrets: &[&SecretValue]) -> Vec<u8> {
    let mut out = data.to_vec();
    for secret in secrets {
        let needle = secret.expose().as_bytes();
        if needle.is_empty() {
            continue;
        }
        let mut masked = Vec::with_capacity(out.len());
        let mut i = 0;
        while i < out.len() {
            if out[i..].starts_with(needle) {
                masked.extend_from_slice(MASK.as_bytes());
                i += needle.len();
            } else {
                masked.push(out[i]);
                i += 1;
            }
        }
        out = masked;
    }
    out
}
