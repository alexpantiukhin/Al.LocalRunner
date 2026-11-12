use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

/// Why an order was refused at intake; carried as `rejected_<name>` (spec 15 leaves the names
/// of `rejected_*` open, they name the cause).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
    /// The order asks for a service the client label may not use (`[[services.allow]]`).
    ServiceNotAllowed,
    /// The order's commands break the `[commands] mode = "named"` rule.
    CommandNotAllowed,
    /// A resource the submitter set for this run (`--mem`, `--cpu`) exceeds the pool.
    OverrideAbovePool,
    /// A `rejected_*` code of a newer minor version, kept as received (full code).
    Other(String),
}

impl RejectReason {
    /// Every rejection reason.
    pub const ALL: [RejectReason; 19] = [
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
        RejectReason::ServiceNotAllowed,
        RejectReason::CommandNotAllowed,
        RejectReason::OverrideAbovePool,
    ];

    /// The full code, `rejected_<name>`.
    pub fn as_str(&self) -> &str {
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
            RejectReason::ServiceNotAllowed => "rejected_service_not_allowed",
            RejectReason::CommandNotAllowed => "rejected_command_not_allowed",
            RejectReason::OverrideAbovePool => "rejected_override_above_pool",
            RejectReason::Other(name) => name.as_str(),
        }
    }
}

/// Reason code of a job ending or an order refusal, visible in history, `explain` and on the
/// dashboard (spec 15).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
    /// The runner had not enough free disk for `disk_mb` of the job, even after the cache
    /// cleanup; the result is `failed`.
    DiskInsufficient,
    /// The runner killed the job for exceeding its declared `mem_mb` or `disk_mb`; the result
    /// is `failed`.
    ResourceExceeded,
    /// Refused at intake, with the cause.
    Rejected(RejectReason),
    /// A code of a newer minor version that is not a rejection; kept as received.
    Other(String),
}

const REJECTED_PREFIX: &str = "rejected_";

const PLAIN: [ReasonCode; 13] = [
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
    ReasonCode::DiskInsufficient,
    ReasonCode::ResourceExceeded,
];

impl ReasonCode {
    /// Every known code: the thirteen known ones and one `rejected_*` per [`RejectReason`];
    /// codes kept as `Other` are not listed.
    pub fn all() -> Vec<ReasonCode> {
        PLAIN
            .iter()
            .cloned()
            .chain(RejectReason::ALL.iter().cloned().map(ReasonCode::Rejected))
            .collect()
    }

    /// The code as written in history and on the wire.
    pub fn as_str(&self) -> &str {
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
            ReasonCode::DiskInsufficient => "disk_insufficient",
            ReasonCode::ResourceExceeded => "resource_exceeded",
            ReasonCode::Rejected(r) => r.as_str(),
            ReasonCode::Other(code) => code.as_str(),
        }
    }

    /// Whether this is a `rejected_*` code, known or from a newer minor version.
    pub fn is_rejection(&self) -> bool {
        self.as_str().starts_with(REJECTED_PREFIX)
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
        if let Some(known) = ReasonCode::all().into_iter().find(|c| c.as_str() == s) {
            return Ok(known);
        }
        if s.is_empty() || s == REJECTED_PREFIX {
            return Err(format!("empty reason code `{s}`"));
        }
        if s.starts_with(REJECTED_PREFIX) {
            return Ok(ReasonCode::Rejected(RejectReason::Other(s.to_string())));
        }
        Ok(ReasonCode::Other(s.to_string()))
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
