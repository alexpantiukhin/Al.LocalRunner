//! Storage contracts, one per owner (spec 14): [`BrokerStore`] for `broker.db` and `logs/`,
//! [`HubStore`] for `hub.db` and the log copies, [`RunnerStore`] for `runner.db`.
//!
//! Every method is atomic: either all of its effect is stored or none. Timestamps are passed
//! in by the caller from its [`crate::Clock`]; a store never reads the time.

mod broker;
mod broker_rows;
mod hub;
mod query;
mod runner;

pub use broker::BrokerStore;
pub use broker_rows::{
    outlives_cancel, Admission, Applied, CodeSource, GithubRunRow, Inbound, Intake, JobKey,
    JobLogRow, JobRecord, JobRow, JobSpecRow, Join, JoinCodeRow, LogAppend, MachineKind,
    MachineRow, OrderRow, PruneLimits, PruneReport, Revoked, RunnerRow, RunnerState, SourceKind,
    StepRow, StepUpdate, Transition, LOG_TRUNCATED_MARK,
};
pub use hub::{ActiveOrder, BrokerLink, HubStore};
pub use query::{
    compute_stats, nearest_rank, ClientUsage, JobFilter, ProfileStats, Spread, Stats, StatsQuery,
    StepStats,
};
pub use runner::{CacheEntry, CacheKey, RunnerStore, ServiceRow, ServiceState};

use lr_core::{JobId, JobState, Timestamp};
use std::fmt;

/// What an outbox entry carries (spec 15.3: one mechanism on hub, broker and runner).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutboxKind {
    /// An order (hub to broker).
    Order,
    /// A cancel (hub to broker).
    Cancel,
    /// The job started (runner to broker, broker to hub).
    Started,
    /// A chunk of job output; the only kind dropped when a broker outbox is over its limit.
    Chunk,
    /// The result of one step.
    StepResult,
    /// The final result of a job.
    JobResult,
    /// Confirmation of a cancel (broker to hub).
    CancelAck,
}

/// One stored outgoing message, waiting for the recipient's acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEntry {
    /// The job it belongs to.
    pub job: JobId,
    /// Sequence number, counted separately per job and direction, from 1.
    pub seq: u64,
    /// What it carries.
    pub kind: OutboxKind,
    /// The serialized message.
    pub payload: String,
    /// When it was stored.
    pub created_at: Timestamp,
}

/// Size of an outbox for the metrics (spec 12.2 `outbox_pending`, `outbox_oldest_age_seconds`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutboxBacklog {
    /// Unacknowledged entries.
    pub pending: u64,
    /// When the oldest of them was stored; `None` when nothing is pending.
    pub oldest: Option<Timestamp>,
}

impl OutboxBacklog {
    /// The backlog of these pending entries.
    pub fn of(entries: &[OutboxEntry]) -> OutboxBacklog {
        OutboxBacklog {
            pending: entries.len() as u64,
            oldest: entries.iter().map(|e| e.created_at).min(),
        }
    }
}

/// Why a store operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The named row does not exist.
    NotFound(String),
    /// A row with this key already exists, or a name is taken.
    Duplicate(String),
    /// The job state change is not allowed (spec 15.1).
    InvalidTransition {
        /// Current state.
        from: JobState,
        /// Requested state.
        to: JobState,
    },
    /// The operation does not fit the row's current state (start before grant, requeue of a
    /// started job).
    Conflict(String),
    /// No join code with this hash.
    CodeUnknown,
    /// The join code is past `expires_at`.
    CodeExpired,
    /// The join code has no uses left.
    CodeUsedUp,
    /// The join code is for another kind of participant.
    CodeKindMismatch,
    /// The join code fixes another name than the one joining.
    CodeNameMismatch,
    /// A received seq skips the next expected one; on an ordered transport with resume this
    /// is a protocol error, not a loss to tolerate.
    SeqGap {
        /// The seq that should have come.
        expected: u64,
        /// The seq that came.
        got: u64,
    },
    /// Storage failure.
    Io(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::NotFound(what) => write!(f, "not found: {what}"),
            StoreError::Duplicate(what) => write!(f, "already exists: {what}"),
            StoreError::InvalidTransition { from, to } => {
                write!(f, "job state {from:?} cannot become {to:?}")
            }
            StoreError::Conflict(what) => write!(f, "conflict: {what}"),
            StoreError::CodeUnknown => f.write_str("unknown join code"),
            StoreError::CodeExpired => f.write_str("join code expired"),
            StoreError::CodeUsedUp => f.write_str("join code used up"),
            StoreError::CodeKindMismatch => f.write_str("join code is for another kind"),
            StoreError::CodeNameMismatch => f.write_str("join code is for another name"),
            StoreError::SeqGap { expected, got } => {
                write!(f, "seq gap: expected {expected}, got {got}")
            }
            StoreError::Io(e) => write!(f, "storage: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}
