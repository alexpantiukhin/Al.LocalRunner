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
}

/// Hub's order. Seq counts per job in the hub-to-broker direction.
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
    pub by: Identity,
    /// `--force`.
    pub force: bool,
    /// Reason given by the user.
    pub reason: Option<String>,
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
}

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

/// A piece of job output.
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
}

/// The order was stored; the hub drops it from its outbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted {
    /// The job.
    pub job: JobId,
    /// Seq of the order this answers.
    pub order_seq: u64,
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

/// The broker confirms a cancel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelAck {
    /// The job.
    pub job: JobId,
    /// Position in the broker-to-hub stream of the job.
    pub seq: u64,
    /// Seq of the cancel this confirms.
    pub cancel_seq: u64,
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
