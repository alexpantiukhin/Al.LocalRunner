use crate::caps::WireCapabilities;
use crate::version::Version;
use lr_core::{JobId, Zone};
use serde::{Deserialize, Serialize};

/// What kind of participant joins a broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A runner.
    Runner,
    /// A hub (a client of the broker).
    Hub,
}

/// Why the receiver refuses to go on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolErrorCode {
    /// The major version of the peer is not supported.
    UnsupportedVersion,
    /// The key is unknown or the proof failed.
    Unauthorized,
    /// The key was revoked or the runner removed.
    KeyRevoked,
    /// The join code is unknown.
    JoinCodeUnknown,
    /// The join code is past its time.
    JoinCodeExpired,
    /// The join code has no uses left.
    JoinCodeUsedUp,
    /// The join code is for another kind of participant.
    JoinCodeKindMismatch,
    /// The join code fixes another name.
    JoinCodeNameMismatch,
    /// The MachineId equals the broker's own.
    MachineIdClash,
    /// An administrative or local-only message arrived over the network.
    AdminOverNetwork,
    /// The document is not a valid message.
    Malformed,
    /// A frame or document over 4 MiB.
    FrameTooLarge,
    /// A received seq skipped the expected one.
    SeqGap,
    /// Too many requests.
    RateLimited,
    /// A runner of this name is already registered by another machine.
    NameTaken,
    /// A code of a newer minor version; never a command.
    #[serde(other)]
    Other,
}

impl ProtocolErrorCode {
    /// Whether a runner receiving it must kill the job's process tree at once, without the
    /// grace period (spec 3.9: a refusal of authorization is a command).
    pub fn kills_running_jobs(self) -> bool {
        matches!(
            self,
            ProtocolErrorCode::Unauthorized | ProtocolErrorCode::KeyRevoked
        )
    }
}

/// A refusal sent instead of going on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    /// Machine-readable cause.
    pub code: ProtocolErrorCode,
    /// Human-readable text.
    pub message: String,
    /// The version the refuser speaks, set for `unsupported_version`.
    pub supported: Option<Version>,
}

/// First message of every connection after the transport handshake: the peer's key was proven
/// by the transport; this announces version and what the peer offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// Protocol version of the sender.
    pub version: Version,
    /// Runner name or hub name.
    pub name: String,
    /// Capabilities of a runner; `None` for a hub.
    pub capabilities: Option<WireCapabilities>,
}

/// Joining a pool with a one-time code (spec 3.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Join {
    /// Protocol version of the sender.
    pub version: Version,
    /// Kind of participant.
    pub role: Role,
    /// The one-time code.
    pub code: String,
    /// Public key created by the joiner; the private half never leaves it.
    pub public_key: String,
    /// Runner name or hub name.
    pub name: String,
    /// MachineId of the joiner.
    pub machine_id: String,
    /// Capabilities of a joining runner.
    pub capabilities: Option<WireCapabilities>,
}

/// Broker's answer to [`Hello`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    /// The version agreed on.
    pub version: Version,
    /// The zone the broker computed for the peer's address.
    pub zone: Option<Zone>,
    /// Heartbeat interval in seconds.
    pub heartbeat_s: u64,
    /// Seconds after which a runner kills its jobs when the broker is unreachable; given at
    /// registration, a runner may only shorten it.
    pub broker_lost_kill_s: Option<u64>,
}

/// Broker's answer to [`Join`]: the code is redeemed and the key registered. It is the only
/// reply on a join connection: no [`Welcome`] follows, so it carries the same session facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Joined {
    /// The version agreed on.
    pub version: Version,
    /// The zone the broker computed for the joiner's address.
    pub zone: Option<Zone>,
    /// Heartbeat interval in seconds.
    pub heartbeat_s: u64,
    /// Seconds after which a runner kills its jobs when the broker is unreachable.
    pub broker_lost_kill_s: Option<u64>,
    /// Identifier of the registered key.
    pub key_id: String,
    /// MachineId of the broker.
    pub broker_machine_id: String,
}

/// The last seq the receiver accepted for one job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumePoint {
    /// The job.
    pub job: JobId,
    /// Last accepted seq, 0 when none.
    pub last_seq: u64,
}

/// After a reconnect the receiver tells per job what it has; the sender resends the rest. A job
/// that is not listed counts as `last_seq = 0`: the sender resends its whole outbox for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resume {
    /// Per job.
    pub points: Vec<ResumePoint>,
}

impl Resume {
    /// The last accepted seq of `job`, 0 when the job is not listed.
    pub fn last_seq(&self, job: &JobId) -> u64 {
        self.points
            .iter()
            .find(|p| &p.job == job)
            .map_or(0, |p| p.last_seq)
    }
}

/// The receiver confirms messages of a job up to and including `seq`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    /// The job.
    pub job: JobId,
    /// Confirmed up to this seq.
    pub seq: u64,
}

/// A runner or hub leaves in an orderly way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bye {
    /// Optional note.
    pub reason: Option<String>,
}

/// State of a runner service (spec 15.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    /// Started, readiness check running.
    Starting,
    /// Warm and free.
    Idle,
    /// Serving a job.
    Busy,
    /// Regular stop in progress.
    Stopping,
    /// No processes left.
    Stopped,
    /// Did not come up or crashed.
    Failed,
    /// Restart limit exhausted.
    Unavailable,
    /// A state of a newer minor version.
    #[serde(other)]
    Unknown,
}

/// One service in a runner's report: who is warm and how much memory it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceReport {
    /// Service name.
    pub name: String,
    /// State.
    pub state: ServiceState,
    /// Memory counted in the pool budget.
    pub mem_mb: u64,
    /// Seconds of `max_lifetime_s` left.
    pub remaining_s: Option<u64>,
}

/// Runner's report on its services.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServicesReport {
    /// All services of the runner.
    pub services: Vec<ServiceReport>,
}

/// The broker pushes changed settings to a connected runner (`[grace]` of the policy, spec 3.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsUpdate {
    /// New seconds after which the runner kills its jobs when the broker is unreachable.
    pub broker_lost_kill_s: Option<u64>,
}

/// A runner reports changed capabilities or cache fill during a session (spec 3.10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitiesUpdate {
    /// The full current capabilities.
    pub capabilities: WireCapabilities,
}

/// Broker's command to stop a warm service (eviction, spec 3.11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopService {
    /// Service name.
    pub name: String,
}
