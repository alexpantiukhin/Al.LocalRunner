use lr_core::{JobId, JobState, StepState, Timestamp};
use std::collections::{BTreeMap, BTreeSet};

/// Mark appended once to a job log cut at its size limit (spec 8: 256 MB per job by default).
pub const LOG_TRUNCATED_MARK: &[u8] = b"\n[localrunner: log truncated at its size limit]\n";

/// Key of a run on the broker and its idempotency key: client key plus `JobId` (spec 2). The
/// client key of a network client is its machine name; of a local client the pair of local
/// record and OS user.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobKey {
    /// Client key.
    pub client: String,
    /// Job id issued by the hub.
    pub job: JobId,
}

impl JobKey {
    /// Builds a key.
    pub fn new(client: &str, job: &str) -> JobKey {
        JobKey {
            client: client.to_string(),
            job: JobId(job.to_string()),
        }
    }
}

/// Where the job came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    /// Terminal or agent session.
    Session,
    /// GitHub Actions job.
    Github,
}

/// Which code the job runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodeSource {
    /// The client's work tree (same machine only).
    WorkTree,
    /// A pushed commit.
    Commit,
}

/// Row of `job`: one run with everything known about it (spec 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRow {
    /// Client key and job id.
    pub key: JobKey,
    /// Project name.
    pub project: String,
    /// Source kind.
    pub source: SourceKind,
    /// Label `--as`.
    pub label: String,
    /// Note of the submitter.
    pub note: Option<String>,
    /// Branch.
    pub branch: Option<String>,
    /// Working directory on the client.
    pub cwd: String,
    /// Profile name.
    pub profile: String,
    /// Platform; `None` for a same-machine job.
    pub platform: Option<String>,
    /// Runner the job was granted to.
    pub runner: Option<String>,
    /// Commit checked out for a commit job.
    pub commit_sha: Option<String>,
    /// MachineId of the client, from its key record.
    pub client_machine: String,
    /// Client label, from its key record.
    pub client_label: String,
    /// OS user of the submitter.
    pub os_user: String,
    /// Names (never values) of the secrets requested.
    pub secrets: Vec<String>,
    /// Code source.
    pub code_source: CodeSource,
    /// When the hub enqueued it.
    pub queued_at: Timestamp,
    /// `Started` on the broker clock.
    pub started_at: Option<Timestamp>,
    /// When it reached its final state.
    pub finished_at: Option<Timestamp>,
    /// Current state.
    pub state: JobState,
    /// Exit code of the run.
    pub exit_code: Option<i32>,
    /// Who cancelled it.
    pub cancelled_by: Option<String>,
    /// Why it was cancelled or ended (reason code and text).
    pub cancel_reason: Option<String>,
}

/// Row of `step`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRow {
    /// Position from 0.
    pub ord: u32,
    /// Step name.
    pub name: String,
    /// Command line.
    pub command: String,
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

/// Row of `job_spec`: the resolved order the broker needs to queue the job again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpecRow {
    /// Resolved profile and order as JSON.
    pub resolved_json: String,
    /// Commit the profile was read from.
    pub profile_commit: Option<String>,
}

/// Row of `github_run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubRunRow {
    /// `owner/repo`.
    pub repository: String,
    /// Workflow name.
    pub workflow: String,
    /// Job name.
    pub job_name: String,
    /// Run id.
    pub run_id: u64,
    /// Attempt.
    pub attempt: Option<u64>,
    /// Git ref.
    pub git_ref: String,
    /// Actor.
    pub actor: String,
    /// Link to the run.
    pub run_url: String,
}

/// Row of `job_log`: where the full log lies and how big it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobLogRow {
    /// Path of `logs/<job-id>.log`.
    pub path: String,
    /// Stored size in bytes.
    pub bytes: u64,
    /// Last append.
    pub updated_at: Timestamp,
}

/// Row of `order`: the queue before grant only; removed when the job is granted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderRow {
    /// Client key and job id.
    pub key: JobKey,
    /// MachineId of the client.
    pub client_machine: String,
    /// The order as JSON.
    pub spec_json: String,
    /// Platform; `None` for a same-machine order.
    pub platform: Option<String>,
    /// Whether it must run on the client's machine.
    pub same_machine: bool,
    /// Priority the order wishes.
    pub priority_hint: Option<i64>,
    /// When the hub enqueued it; kept on re-brokering.
    pub enqueued_at: Timestamp,
    /// Queue state as the engine names it; not interpreted by the store.
    pub state: String,
}

/// Everything stored when an order is accepted, in one atomic write; `Accepted` is sent only
/// after it (spec 3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intake {
    /// The run record, state `Queued`.
    pub job: JobRow,
    /// Its steps, state `Queued`.
    pub steps: Vec<StepRow>,
    /// Its resolved spec.
    pub spec: JobSpecRow,
    /// GitHub facts for a `github` job.
    pub github: Option<GithubRunRow>,
    /// The queue row; its key equals the job key.
    pub order: OrderRow,
}

/// A run with all its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRecord {
    /// The run.
    pub job: JobRow,
    /// Its steps by `ord`.
    pub steps: Vec<StepRow>,
    /// Its resolved spec.
    pub spec: JobSpecRow,
    /// GitHub facts.
    pub github: Option<GithubRunRow>,
    /// Its log, if any was written and not pruned.
    pub log: Option<JobLogRow>,
}

/// Result of storing an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Stored now.
    Accepted,
    /// The same client key and `JobId` were stored before; nothing changed.
    Duplicate,
}

/// Result of an update that may arrive late.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// The row changed.
    Changed,
    /// The job already has its final state, or the step already ended; nothing changed.
    Ignored,
}

/// A job state change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    /// New state.
    pub state: JobState,
    /// When; becomes `started_at` for `Running` and `finished_at` for a final state.
    pub at: Timestamp,
    /// Exit code reported with a final state.
    pub exit_code: Option<i32>,
    /// Who cancelled, kept once set.
    pub cancelled_by: Option<String>,
    /// Reason, kept once set.
    pub cancel_reason: Option<String>,
}

impl Transition {
    /// A change to `state` at `at` with nothing else.
    pub fn to(state: JobState, at: Timestamp) -> Transition {
        Transition {
            state,
            at,
            exit_code: None,
            cancelled_by: None,
            cancel_reason: None,
        }
    }
}

/// A step state change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepUpdate {
    /// Step position.
    pub ord: u32,
    /// New state.
    pub state: StepState,
    /// Exit code.
    pub exit_code: Option<i32>,
    /// Start, kept when `None`.
    pub started_at: Option<Timestamp>,
    /// End, kept when `None`.
    pub finished_at: Option<Timestamp>,
}

/// Result of appending to a job log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogAppend {
    /// Stored whole.
    Appended,
    /// The limit was reached now: the part that fits and [`LOG_TRUNCATED_MARK`] were stored.
    Truncated,
    /// The log was already cut; nothing stored.
    Dropped,
}

/// Kind of a registered participant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MachineKind {
    /// A client hub.
    Hub,
    /// A runner.
    Runner,
}

/// Row of `machine`: a key record. Only public keys and labels; no tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRow {
    /// Unique name on the broker; also the client key of a hub.
    pub name: String,
    /// MachineId declared at join.
    pub machine_id: String,
    /// Kind.
    pub kind: MachineKind,
    /// Label set by the broker owner.
    pub label: String,
    /// Public key.
    pub public_key: String,
    /// When it joined.
    pub registered_at: Timestamp,
    /// Last heartbeat or connection.
    pub last_seen: Option<Timestamp>,
    /// When the key was revoked.
    pub revoked_at: Option<Timestamp>,
}

/// Row of `join_code`. Only the hash of the code is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinCodeRow {
    /// Hash of the code.
    pub code_hash: String,
    /// Who may join with it.
    pub kind: MachineKind,
    /// Client name fixed by `broker add --name`.
    pub name: Option<String>,
    /// Label given to the joining key.
    pub label: Option<String>,
    /// Joins left.
    pub uses_left: u32,
    /// Issue time.
    pub issued_at: Timestamp,
    /// The code is refused from this moment on.
    pub expires_at: Timestamp,
    /// When the last use was spent.
    pub redeemed_at: Option<Timestamp>,
    /// Machine of the latest join.
    pub machine_name: Option<String>,
}

/// A join with a code, recorded atomically with spending one use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Join {
    /// Hash of the presented code.
    pub code_hash: String,
    /// When.
    pub at: Timestamp,
    /// The key record to create.
    pub machine: MachineRow,
}

/// What a revocation removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revoked {
    /// Runners of the machine now `Removed`.
    pub runners: Vec<String>,
    /// Entries of the machine's result outbox deleted.
    pub outbox_removed: usize,
}

/// State of a runner (spec 15.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunnerState {
    /// Joined, not connected.
    Registered,
    /// Heartbeats arrive.
    Online,
    /// Three heartbeats missed.
    Offline,
    /// Finishes its job, takes no new ones.
    Draining,
    /// Its key was revoked.
    Removed,
}

/// Row of `runner`: a registered runner and what it reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerRow {
    /// Unique name on the broker.
    pub name: String,
    /// Machine (key record) it belongs to.
    pub machine_name: String,
    /// Programs found and checked.
    pub programs: BTreeSet<String>,
    /// Platform labels.
    pub platforms: BTreeSet<String>,
    /// Tags.
    pub tags: BTreeSet<String>,
    /// `max_job_timeout_s`.
    pub max_timeout_s: Option<u64>,
    /// Cache size limit.
    pub cache_max_mb: u64,
    /// Cache used.
    pub cache_used_mb: u64,
    /// Free disk.
    pub free_disk_mb: u64,
    /// Declared services and whether each is available.
    pub services: BTreeMap<String, bool>,
    /// `[accept] min_mem_mb`.
    pub accept_min_mem_mb: Option<u64>,
    /// Last heartbeat.
    pub last_seen: Option<Timestamp>,
    /// State.
    pub state: RunnerState,
}

/// History retention (spec 8): by age and total log size, whichever comes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PruneLimits {
    /// Current time.
    pub now: Timestamp,
    /// Logs of jobs finished longer ago than this are removed (default 30 days).
    pub log_max_age_ms: u64,
    /// While all logs together are bigger, logs of the oldest finished jobs are removed
    /// (default 2 GB).
    pub log_max_bytes: u64,
    /// Rows of jobs finished longer ago than this are removed (default 365 days).
    pub row_max_age_ms: u64,
}

/// What a prune removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// Jobs whose log was removed, by any rule.
    pub logs_removed: usize,
    /// Jobs removed with all their rows.
    pub rows_removed: usize,
    /// Log bytes freed.
    pub bytes_freed: u64,
}
