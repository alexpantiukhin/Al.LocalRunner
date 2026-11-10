use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

/// Why an order was refused at intake; carried as `rejected_<name>` (spec 15 leaves the names
/// of `rejected_*` open, they name the cause).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RejectReason {
    /// Needs more memory than the whole pool.
    PoolMemory,
    /// Needs more CPU than the whole pool.
    PoolCpu,
    /// Timeout above the broker limit.
    Timeout,
    /// Policy reserves leave the pool too little.
    Reserved,
    /// The quota of the group is below the order.
    Quota,
    /// No registered runner suits the order.
    NoMatchingRunner,
    /// Same-machine order and no runner of that installation online.
    NoRunnerOnMachine,
    /// A secret is not allowed to the client label.
    SecretNotAllowed,
    /// No broker of the allowed zones.
    ZoneNotServed,
    /// Same-machine order and no local broker in the hub list.
    NoLocalBroker,
    /// The OS user is not in `[[local_user]]`.
    LocalUserUnknown,
    /// Brokers did not answer within the attempts.
    BrokersUnreachable,
    /// The outbox time-to-live ran out with `on_expire = fail`.
    OutboxExpired,
    /// The client has too many active orders.
    QueueLimit,
    /// API rate limit.
    RateLimited,
    /// A remote order whose commit is not pushed.
    CommitNotPushed,
}

impl RejectReason {
    /// Every rejection reason.
    pub const ALL: [RejectReason; 16] = [
        RejectReason::PoolMemory,
        RejectReason::PoolCpu,
        RejectReason::Timeout,
        RejectReason::Reserved,
        RejectReason::Quota,
        RejectReason::NoMatchingRunner,
        RejectReason::NoRunnerOnMachine,
        RejectReason::SecretNotAllowed,
        RejectReason::ZoneNotServed,
        RejectReason::NoLocalBroker,
        RejectReason::LocalUserUnknown,
        RejectReason::BrokersUnreachable,
        RejectReason::OutboxExpired,
        RejectReason::QueueLimit,
        RejectReason::RateLimited,
        RejectReason::CommitNotPushed,
    ];

    /// The full code, `rejected_<name>`.
    pub fn as_str(self) -> &'static str {
        match self {
            RejectReason::PoolMemory => "rejected_pool_memory",
            RejectReason::PoolCpu => "rejected_pool_cpu",
            RejectReason::Timeout => "rejected_timeout",
            RejectReason::Reserved => "rejected_reserved",
            RejectReason::Quota => "rejected_quota",
            RejectReason::NoMatchingRunner => "rejected_no_matching_runner",
            RejectReason::NoRunnerOnMachine => "rejected_no_runner_on_machine",
            RejectReason::SecretNotAllowed => "rejected_secret_not_allowed",
            RejectReason::ZoneNotServed => "rejected_zone_not_served",
            RejectReason::NoLocalBroker => "rejected_no_local_broker",
            RejectReason::LocalUserUnknown => "rejected_local_user_unknown",
            RejectReason::BrokersUnreachable => "rejected_brokers_unreachable",
            RejectReason::OutboxExpired => "rejected_outbox_expired",
            RejectReason::QueueLimit => "rejected_queue_limit",
            RejectReason::RateLimited => "rejected_rate_limited",
            RejectReason::CommitNotPushed => "rejected_commit_not_pushed",
        }
    }
}

/// Reason code of a job ending or an order refusal, visible in history, `explain` and on the
/// dashboard (spec 15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReasonCode {
    /// The runner was stopped by `down --cancel`.
    RunnerStopped,
    /// The runner was removed.
    RunnerRemoved,
    /// The broker did not return within the grace period; the job was aborted.
    BrokerUnavailable,
    /// The runner vanished and the time ran out.
    RunnerLost,
    /// The client key was revoked.
    ClientRemoved,
    /// The reaper cancelled the order of a dead sender.
    SenderGone,
    /// The hub was absent longer than `absent_after_min`.
    HubAbsent,
    /// A runner service did not come up.
    ServiceFailed,
    /// A requested secret source is unavailable.
    SecretMissing,
    /// The runner restarted with a live job; the result is `failed`.
    RunnerRestarted,
    /// Cancelled by the user.
    CancelledByUser,
    /// Refused at intake, with the cause.
    Rejected(RejectReason),
}

const PLAIN: [ReasonCode; 11] = [
    ReasonCode::RunnerStopped,
    ReasonCode::RunnerRemoved,
    ReasonCode::BrokerUnavailable,
    ReasonCode::RunnerLost,
    ReasonCode::ClientRemoved,
    ReasonCode::SenderGone,
    ReasonCode::HubAbsent,
    ReasonCode::ServiceFailed,
    ReasonCode::SecretMissing,
    ReasonCode::RunnerRestarted,
    ReasonCode::CancelledByUser,
];

impl ReasonCode {
    /// Every code: the eleven of spec 15 and one `rejected_*` per [`RejectReason`].
    pub fn all() -> Vec<ReasonCode> {
        PLAIN
            .iter()
            .copied()
            .chain(RejectReason::ALL.iter().map(|r| ReasonCode::Rejected(*r)))
            .collect()
    }

    /// The code as written in history and on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            ReasonCode::RunnerStopped => "runner_stopped",
            ReasonCode::RunnerRemoved => "runner_removed",
            ReasonCode::BrokerUnavailable => "broker_unavailable",
            ReasonCode::RunnerLost => "runner_lost",
            ReasonCode::ClientRemoved => "client_removed",
            ReasonCode::SenderGone => "sender_gone",
            ReasonCode::HubAbsent => "hub_absent",
            ReasonCode::ServiceFailed => "service_failed",
            ReasonCode::SecretMissing => "secret_missing",
            ReasonCode::RunnerRestarted => "runner_restarted",
            ReasonCode::CancelledByUser => "cancelled_by_user",
            ReasonCode::Rejected(r) => r.as_str(),
        }
    }

    /// Whether this is a `rejected_*` code.
    pub fn is_rejection(self) -> bool {
        matches!(self, ReasonCode::Rejected(_))
    }
}

impl fmt::Display for ReasonCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReasonCode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        ReasonCode::all()
            .into_iter()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| format!("unknown reason code `{s}`"))
    }
}

impl Serialize for ReasonCode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ReasonCode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        text.parse().map_err(D::Error::custom)
    }
}
