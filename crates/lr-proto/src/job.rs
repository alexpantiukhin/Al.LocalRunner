use crate::frame::FRAME_LIMIT;
use crate::reason::ReasonCode;
use lr_core::{Identity, JobId, JobState, Requirements, StepSpec, StepState, Timestamp};
use serde::{Deserialize, Serialize};

/// Which code a job runs (spec 3.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireCode {
    /// The client's work tree, same machine only.
    WorkTree {
        /// Working directory of the submitter.
        cwd: String,
    },
    /// A pushed commit.
    Commit {
        /// Commit hash.
        sha: String,
        /// Address of the repository (`git remote get-url origin`) the runner mirrors and
        /// fetches; absent from senders of an older minor version.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo_url: Option<String>,
        /// Branch of the submitter, for the `{branch}` placeholder of a profile `source` step.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
}

/// The order as the hub sends it (spec 3.4): data, never code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderSpec {
    /// Profile name.
    pub profile: String,
    /// What the job needs from a runner.
    pub requirements: Requirements,
    /// Steps with timeouts.
    pub steps: Vec<StepSpec>,
    /// When the hub enqueued it; kept on re-brokering.
    pub enqueued_at: Timestamp,
    /// Who submitted it.
    pub identity: Identity,
    /// Code source.
    pub code: WireCode,
    /// Wished priority; the policy decides.
    pub wanted_priority: Option<i64>,
    /// Resources the submitter set for this run (`--mem`, `--cpu`); the requirements already
    /// carry them. Absent from older senders and when nothing was overridden.
    #[serde(default, skip_serializing_if = "ResourceOverride::is_empty")]
    pub overrides: ResourceOverride,
    /// Profile arguments the submitter gave (`--arg`); the steps already carry their values.
    /// Absent from older senders and when none were given.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<(String, String)>,
    /// Labels of the profile; the policy reads them, a runner never does. Absent from older
    /// senders and when the profile has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

/// Per-run replacement of profile resources given by the submitter (spec 5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceOverride {
    /// `--mem` in MB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_mb: Option<u64>,
    /// `--cpu`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<u32>,
}

impl ResourceOverride {
    /// Whether nothing is overridden.
    pub fn is_empty(&self) -> bool {
        self.mem_mb.is_none() && self.cpu.is_none()
    }
}

/// Hub's order. It is seq 1 of the job's hub-to-broker stream at the broker that accepts it;
/// when an order is rejected or returned and goes to another broker, the stream for that broker
/// restarts at 1 (see [`crate::SeqStream::restart`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    /// Job id issued by the hub.
    pub job: JobId,
    /// Position in the hub-to-broker stream of the job.
    pub seq: u64,
    /// The order.
    pub spec: Box<OrderSpec>,
}

/// Hub's cancel of a job (spec 7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubCancel {
    /// The job.
    pub job: JobId,
    /// Position in the hub-to-broker stream of the job.
    pub seq: u64,
    /// Who cancels, for the key and ownership rule.
    pub by: Box<Identity>,
    /// `--force`.
    pub force: bool,
    /// Why: `cancelled_by_user`, `sender_gone`, ...
    pub code: ReasonCode,
    /// Free text given by the user.
    pub reason: Option<String>,
    /// Client key of the run, when the canceller is the broker owner and several clients use
    /// this JobId; without it the own run is meant, else the only other one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
}

/// What the broker gives a runner to execute: admitted secret names only, never values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssignSpec {
    /// Client key, part of the cache key.
    pub client: String,
    /// Project.
    pub project: String,
    /// Requirements, with services, cache names and timeout.
    pub requirements: Requirements,
    /// Steps.
    pub steps: Vec<StepSpec>,
    /// Secret names the policy admitted.
    pub secrets: Vec<String>,
    /// Code source.
    pub code: WireCode,
    /// Seconds after which the runner kills the job if the broker is unreachable.
    pub broker_lost_kill_s: Option<u64>,
    /// Set for a client limited to named commands (`[commands] mode = "named"`, spec 3.5): the
    /// command of every step is a name the runner resolves through its own `[commands]`, never
    /// a command line to run. Absent from brokers of an earlier minor version.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub named_commands: bool,
    /// Client part of the cache key when it is not `client`: [`SHARED_CACHE_CLIENT`] under
    /// `[cache] share = "project"`, so the clients of one project share their named caches.
    /// Absent: the cache belongs to `client` alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_client: Option<String>,
    /// Resource enforcement the broker policy asks for; absent from brokers of an earlier
    /// minor version and when `[resources]` is not set: the runner then only records peaks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<WireLimits>,
}

/// How a runner treats a job that exceeds its declared resources.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireEnforce {
    /// Only the peaks are recorded.
    #[default]
    Off,
    /// A warning goes to the job log.
    Warn,
    /// The job is killed.
    Kill,
}

/// The `[resources]` part of the broker policy that travels with an assignment: the ceiling of
/// a job is its declared value times the headroom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireLimits {
    /// What happens above the ceiling.
    pub mode: WireEnforce,
    /// Memory ceiling in percent of the declared `mem_mb` (125 is the declared value plus a
    /// quarter).
    pub mem_headroom_pct: u32,
    /// Disk ceiling in percent of the declared `disk_mb`.
    pub disk_headroom_pct: u32,
}

/// Client part of the cache key the broker sends under `[cache] share = "project"`: the same
/// for every client, so the cache key is the project and the cache name. It is neither a
/// public key (`ed25519:`) nor a local client key (`local:`).
pub const SHARED_CACHE_CLIENT: &str = "shared:project";

/// Broker's assignment of a job to a runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assign {
    /// The job.
    pub job: JobId,
    /// Position in the broker-to-runner stream of the job.
    pub seq: u64,
    /// What to run.
    pub spec: Box<AssignSpec>,
}

/// Broker's cancel to a runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerCancel {
    /// The job.
    pub job: JobId,
    /// Position in the broker-to-runner stream of the job.
    pub seq: u64,
    /// Why.
    pub code: ReasonCode,
    /// Text for the log.
    pub detail: Option<String>,
}

/// The job started; the broker stamps `started_at` from its own clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Started {
    /// The job.
    pub job: JobId,
    /// Position in the stream of the job.
    pub seq: u64,
}

/// Reserve for the JSON envelope of a chunk (job id, seq, step, keys).
pub const CHUNK_ENVELOPE_RESERVE: usize = 64 * 1024;

/// Largest `Chunk.text` in bytes. JSON may escape one byte into six (`\u00XX`), so the worst
/// case still fits a frame together with [`CHUNK_ENVELOPE_RESERVE`].
pub const CHUNK_TEXT_LIMIT: usize = (FRAME_LIMIT - CHUNK_ENVELOPE_RESERVE) / 6;

/// Splits output into pieces of at most `limit` bytes, never inside a character. A `limit`
/// smaller than one character yields that character alone.
pub fn split_text(text: &str, limit: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut end = limit.min(rest.len());
        while end > 0 && !rest.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            end = rest.chars().next().map_or(rest.len(), char::len_utf8);
        }
        let (head, tail) = rest.split_at(end);
        out.push(head);
        rest = tail;
    }
    out
}

pub(crate) fn check_chunk(c: &Chunk) -> Result<(), String> {
    crate::seq::seq_from_one(c.seq)?;
    if c.text.len() > CHUNK_TEXT_LIMIT {
        return Err(format!(
            "chunk text of {} bytes exceeds {CHUNK_TEXT_LIMIT}",
            c.text.len()
        ));
    }
    Ok(())
}

/// A piece of job output. `text` is lossy UTF-8: the runner decodes the stream incrementally,
/// masks secrets in the bytes before the lossy conversion, and cuts pieces of at most
/// [`CHUNK_TEXT_LIMIT`] bytes at character boundaries ([`split_text`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    /// The job.
    pub job: JobId,
    /// Position in the stream of the job.
    pub seq: u64,
    /// Step the output belongs to, by position.
    pub step: Option<u32>,
    /// Output as text, secrets already masked.
    pub text: String,
}

/// Result of one step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepResult {
    /// The job.
    pub job: JobId,
    /// Position in the stream of the job.
    pub seq: u64,
    /// Step position from 0.
    pub ord: u32,
    /// Step name.
    pub name: String,
    /// Final state of the step.
    pub state: StepState,
    /// Exit code.
    pub exit_code: Option<i32>,
    /// Duration measured by the runner.
    pub duration_ms: u64,
}

/// Final result of a job; the only source of its final state once it ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobResult {
    /// The job.
    pub job: JobId,
    /// Position in the stream of the job.
    pub seq: u64,
    /// A final state.
    pub state: JobState,
    /// Exit code.
    pub exit_code: Option<i32>,
    /// Reason code of a non-regular end.
    pub reason: Option<ReasonCode>,
    /// Text for the log.
    pub detail: Option<String>,
    /// Peak memory of the job's process tree in MB; absent from runners of an earlier minor
    /// version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_mem_mb: Option<u64>,
    /// Peak size of the job's working and temporary directories in MB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_disk_mb: Option<u64>,
}

/// The order was stored; the hub drops it from its outbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted {
    /// The job.
    pub job: JobId,
    /// Seq of the order this answers.
    pub order_seq: u64,
    /// Why the order waits right away (runner busy, memory, rule), if it does.
    pub waiting: Option<String>,
    /// The timeout the order runs with when the policy trimmed a longer one to its maximum
    /// (`[limits] timeout_above_max = "trim"`); absent from older brokers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_trimmed_to: Option<u64>,
}

/// The order was refused at intake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejected {
    /// The job.
    pub job: JobId,
    /// Seq of the order this answers.
    pub order_seq: u64,
    /// A `rejected_*` code.
    pub code: ReasonCode,
    /// Text with the figures.
    pub detail: String,
}

/// What the broker did with a cancel (spec 7.1, scenario 16.17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelOutcome {
    /// The cancel was accepted: a waiting order is removed, a running job is being stopped.
    Applied,
    /// The key or ownership rule refused it.
    Denied,
    /// The job already had its final state; nothing changed.
    AlreadyFinal,
    /// An outcome of a newer minor version.
    #[serde(other)]
    Unknown,
}

/// The broker answers a cancel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelAck {
    /// The job.
    pub job: JobId,
    /// Position in the broker-to-hub stream of the job; 0 for a `denied` answer sent outside
    /// the outbox, which is not part of the stream.
    pub seq: u64,
    /// Seq of the cancel this answers.
    pub cancel_seq: u64,
    /// What happened.
    pub outcome: CancelOutcome,
    /// Why it was denied.
    pub denial: Option<String>,
}

/// The job started, as the broker tells the hub: the start time is the broker's own clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartedNotice {
    /// The job.
    pub job: JobId,
    /// Position in the broker-to-hub stream of the job.
    pub seq: u64,
    /// Runner that executes it.
    pub runner: String,
    /// Start on the broker clock.
    pub started_at: Timestamp,
}

/// The broker gives back an accepted order that has not started, with the cause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Returned {
    /// The job.
    pub job: JobId,
    /// Position in the broker-to-hub stream of the job.
    pub seq: u64,
    /// Why.
    pub code: ReasonCode,
    /// Text.
    pub detail: Option<String>,
}
