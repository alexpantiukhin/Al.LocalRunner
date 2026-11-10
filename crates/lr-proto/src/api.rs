use crate::caps::WireCapabilities;
use crate::common::ServiceReport;
use crate::reason::ReasonCode;
use lr_core::{JobId, JobState, StepState, Timestamp};
use serde::{Deserialize, Serialize};

/// Where a job came from, as a filter value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireSource {
    /// Terminal or agent session.
    Session,
    /// GitHub Actions job.
    Github,
}

/// Filter of the `status` view.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusFilter {
    /// Project.
    pub project: Option<String>,
    /// Source kind.
    pub source: Option<WireSource>,
    /// Label `--as`.
    pub label: Option<String>,
}

/// Filter of `history` (spec 8); every set field must match.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryFilter {
    /// Client key.
    pub client: Option<String>,
    /// Project.
    pub project: Option<String>,
    /// Source kind.
    pub source: Option<WireSource>,
    /// Label `--as`.
    pub label: Option<String>,
    /// Profile.
    pub profile: Option<String>,
    /// State.
    pub state: Option<JobState>,
    /// Platform.
    pub platform: Option<String>,
    /// Runner.
    pub runner: Option<String>,
    /// Only runs queued at or after this time.
    pub since: Option<Timestamp>,
    /// Only runs that are not final.
    pub active_only: bool,
    /// Maximum number of runs.
    pub limit: Option<u64>,
}

/// Filter of `stats` (spec 8).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatsFilter {
    /// Client key.
    pub client: Option<String>,
    /// Project.
    pub project: Option<String>,
    /// Profile.
    pub profile: Option<String>,
    /// Only runs queued at or after this time.
    pub since: Option<Timestamp>,
}

/// What a subscription delivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// Queue and job changes.
    Queue,
    /// Runner and service changes.
    Runners,
}

/// Read requests of the CLI and the dashboard (spec 3.9, 9.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReadRequest {
    /// Queue and running jobs with steps and wait reasons.
    Status(StatusFilter),
    /// Finished and active runs.
    History(HistoryFilter),
    /// The full record of one run.
    HistoryShow {
        /// The job.
        job: JobId,
    },
    /// Why a job waits.
    Explain {
        /// The job.
        job: JobId,
    },
    /// The stored log; with `follow` a running job's log is streamed as `log_chunk`.
    Log {
        /// The job.
        job: JobId,
        /// Only this step.
        step: Option<String>,
        /// Only the last lines.
        tail: Option<u64>,
        /// Keep streaming.
        follow: bool,
    },
    /// Statistics and saturation.
    Stats(StatsFilter),
    /// Runners with load, tags, services and cache.
    Runners,
    /// Subscribe to events.
    Subscribe {
        /// What to deliver.
        topics: Vec<Topic>,
    },
    /// Cancel a subscription.
    Unsubscribe {
        /// Subscription id.
        sub: u64,
    },
}

/// A run in a list. Fields that the broker's visibility policy hides from other clients are
/// `None` (spec 3.2: `summary` shows id, state, platform, runner, times and client label).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSummary {
    /// The job.
    pub job: JobId,
    /// Label of the client key.
    pub client_label: String,
    /// State.
    pub state: JobState,
    /// Platform.
    pub platform: Option<String>,
    /// Runner.
    pub runner: Option<String>,
    /// When it was queued.
    pub queued_at: Timestamp,
    /// Start on the broker clock.
    pub started_at: Option<Timestamp>,
    /// End.
    pub finished_at: Option<Timestamp>,
    /// Exit code.
    pub exit_code: Option<i32>,
    /// Reason code of a non-regular end.
    pub reason: Option<ReasonCode>,
    /// Project; hidden at `summary`.
    pub project: Option<String>,
    /// Label `--as`; hidden at `summary`.
    pub label: Option<String>,
    /// Profile; hidden at `summary`.
    pub profile: Option<String>,
}

/// A step of a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepView {
    /// Position from 0.
    pub ord: u32,
    /// Name.
    pub name: String,
    /// State.
    pub state: StepState,
    /// Exit code.
    pub exit_code: Option<i32>,
    /// Start.
    pub started_at: Option<Timestamp>,
    /// End.
    pub finished_at: Option<Timestamp>,
    /// Step timeout.
    pub timeout_s: u64,
}

/// The full record of a run (`history show`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobDetail {
    /// The summary.
    pub summary: JobSummary,
    /// Note of the submitter.
    pub note: Option<String>,
    /// Branch.
    pub branch: Option<String>,
    /// Working directory on the client.
    pub cwd: Option<String>,
    /// Commit of a commit job.
    pub commit: Option<String>,
    /// OS user of the submitter.
    pub os_user: Option<String>,
    /// Source kind.
    pub source: Option<WireSource>,
    /// Link to the GitHub run.
    pub github_run_url: Option<String>,
    /// Steps.
    pub steps: Vec<StepView>,
    /// Names of the secrets requested, never values.
    pub secrets: Vec<String>,
    /// Who cancelled it.
    pub cancelled_by: Option<String>,
    /// Why it waits, for a queued job.
    pub wait_reason: Option<String>,
}

/// A running job with steps and the tail of the current step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningView {
    /// The run.
    pub summary: JobSummary,
    /// Steps.
    pub steps: Vec<StepView>,
    /// Last lines of the current step.
    pub last_lines: Vec<String>,
}

/// A queued order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueItem {
    /// The order as a run.
    pub summary: JobSummary,
    /// Position in the queue from 1.
    pub position: u32,
    /// Why it waits.
    pub wait_reason: Option<String>,
}

/// Pool load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolLoad {
    /// Memory in use.
    pub used_mem_mb: u64,
    /// Pool memory.
    pub total_mem_mb: u64,
    /// CPU in use.
    pub used_cpu: u32,
    /// Pool CPU.
    pub total_cpu: u32,
}

/// The `status` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusView {
    /// Running jobs.
    pub running: Vec<RunningView>,
    /// Queue, head first.
    pub queue: Vec<QueueItem>,
    /// Latest finished jobs.
    pub recent: Vec<JobSummary>,
    /// Pool load.
    pub pool: PoolLoad,
}

/// A tag held by a running job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldTag {
    /// Tag.
    pub tag: String,
    /// Holder.
    pub job: JobId,
    /// Whether nobody else may hold it.
    pub exclusive: bool,
}

/// A runner on the runners page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerInfo {
    /// Name.
    pub name: String,
    /// Online or offline.
    pub online: bool,
    /// What it offers, with cache figures.
    pub caps: WireCapabilities,
    /// Last activity.
    pub last_seen: Option<Timestamp>,
    /// Free memory in the pool budget.
    pub free_mem_mb: u64,
    /// Free CPU in the pool budget.
    pub free_cpu: u32,
    /// Tags held now.
    pub held_tags: Vec<HeldTag>,
    /// Live services.
    pub services: Vec<ServiceReport>,
    /// Characteristics that failed the check.
    pub unmet: Vec<String>,
}

/// Median and 95th percentile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpreadView {
    /// 50th percentile.
    pub median: Option<u64>,
    /// 95th percentile.
    pub p95: Option<u64>,
}

/// Statistics of one step name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepStatsView {
    /// Step name.
    pub name: String,
    /// Steps that started.
    pub runs: u64,
    /// Steps that passed.
    pub passed: u64,
    /// Duration in ms.
    pub duration_ms: SpreadView,
}

/// Statistics of one profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileStatsView {
    /// Profile name.
    pub profile: String,
    /// Final runs.
    pub runs: u64,
    /// Passed.
    pub passed: u64,
    /// Failed.
    pub failed: u64,
    /// Timed out.
    pub timeouts: u64,
    /// Cancelled.
    pub cancelled: u64,
    /// Lost.
    pub lost: u64,
    /// Wait in ms.
    pub wait_ms: SpreadView,
    /// Duration in ms.
    pub duration_ms: SpreadView,
    /// Per step.
    pub steps: Vec<StepStatsView>,
}

/// How much of the pool a client used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientUsageView {
    /// Client key.
    pub client: String,
    /// Client label.
    pub label: String,
    /// Final runs.
    pub runs: u64,
    /// Sum of run durations in ms.
    pub busy_ms: u64,
}

/// The `stats` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatsView {
    /// Per profile.
    pub profiles: Vec<ProfileStatsView>,
    /// Per client.
    pub clients: Vec<ClientUsageView>,
}

/// Answers to read requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReadReply {
    /// Answer to `status`.
    Status(StatusView),
    /// Answer to `history`.
    History {
        /// Runs, newest first.
        jobs: Vec<JobSummary>,
    },
    /// Answer to `history_show`.
    Job(Box<JobDetail>),
    /// Answer to `explain`.
    Explain {
        /// The job.
        job: JobId,
        /// The explanation, naming the rule that fired.
        text: String,
    },
    /// Answer to `stats`.
    Stats(StatsView),
    /// Answer to `runners`.
    Runners {
        /// The runners.
        runners: Vec<RunnerInfo>,
    },
    /// A subscription is open.
    Subscribed {
        /// Subscription id.
        sub: u64,
    },
    /// A subscription is closed.
    Unsubscribed {
        /// Subscription id.
        sub: u64,
    },
    /// The job or log does not exist (or is not visible).
    NotFound {
        /// What was asked for.
        what: String,
    },
    /// The visibility policy or key rule refuses.
    Denied {
        /// Why.
        reason: String,
    },
    /// The broker could not answer.
    Failed {
        /// Why.
        message: String,
    },
}

/// A change delivered to subscribers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QueueEvent {
    /// An order entered the queue.
    JobQueued {
        /// The run.
        job: JobSummary,
    },
    /// A job started.
    JobStarted {
        /// The job.
        job: JobId,
        /// Its runner.
        runner: String,
    },
    /// A step changed.
    StepChanged {
        /// The job.
        job: JobId,
        /// The step.
        step: StepView,
    },
    /// A job reached its final state.
    JobFinished {
        /// The job.
        job: JobId,
        /// Final state.
        state: JobState,
        /// Reason code.
        reason: Option<ReasonCode>,
    },
    /// A runner came online.
    RunnerOnline {
        /// Runner name.
        name: String,
    },
    /// A runner went offline.
    RunnerOffline {
        /// Runner name.
        name: String,
    },
    /// A service changed.
    ServiceChanged {
        /// Runner name.
        runner: String,
        /// The service.
        service: ServiceReport,
    },
}
