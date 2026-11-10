use crate::api::{JobSummary, ReadReply, ReadRequest};
use crate::common::ProtocolError;
use crate::frame::message;
use crate::reason::ReasonCode;
use crate::version::Version;
use lr_core::{Identity, JobId, JobState, Timestamp, Zone};
use serde::{Deserialize, Serialize};

/// Which local channel a connection is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpcChannel {
    /// CLI to hub.
    Cli,
    /// Administrative commands to a broker.
    Admin,
    /// Control channel of a runner.
    RunnerControl,
}

/// First message on a local IPC connection: the protocol version and the channel it is for, so
/// a client cannot talk to the wrong endpoint by accident.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcOpen {
    /// Open a channel.
    Hello {
        /// Protocol version of the client.
        version: Version,
        /// The channel wanted.
        channel: IpcChannel,
    },
}

/// Answer to [`IpcOpen`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcOpenReply {
    /// The channel is open at this version.
    Ready {
        /// The version agreed on.
        version: Version,
    },
    /// Wrong version or channel.
    Refused(ProtocolError),
}

/// The CLI places an order through its hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitRequest {
    /// Collected by the CLI; not overridable by flags.
    pub identity: Identity,
    /// Profile name; the default profile when absent.
    pub profile: Option<String>,
    /// The single platform of the job.
    pub platform: Option<String>,
    /// `--timeout`.
    pub timeout_s: Option<u64>,
    /// `--mem` in MB: replaces the profile memory for this run; absent from older senders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_mb: Option<u64>,
    /// `--cpu`: replaces the profile CPU for this run; absent from older senders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<u32>,
    /// `--detach`: the order outlives the sender.
    pub detach: bool,
    /// `--wait`: stream output and end with the exit code.
    pub wait: bool,
}

/// What a cancel names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelTarget {
    /// One job.
    Job(JobId),
    /// All active jobs of the actor (`cancel --mine`).
    Mine,
}

/// The CLI cancels through its hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelRequest {
    /// Who cancels.
    pub actor: Identity,
    /// Which jobs.
    pub target: CancelTarget,
    /// `--force`.
    pub force: bool,
    /// `--reason`, written to the log.
    pub reason: Option<String>,
}

/// A read request relayed by the hub to one broker or to all it knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliRead {
    /// Broker name; all brokers when absent.
    pub broker: Option<String>,
    /// What is asked.
    pub request: ReadRequest,
}

/// A broker in the hub's list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerStatus {
    /// Name in the hub's list.
    pub name: String,
    /// Last known zone.
    pub zone: Option<Zone>,
    /// Whether the broker is reachable.
    pub available: bool,
    /// Messages waiting in the outbox for it.
    pub outbox: u64,
}

/// Messages from the CLI to its hub over local IPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CliRequest {
    /// Place an order.
    Submit(SubmitRequest),
    /// `submit` got its JobId and ends; the hub remembers the sender process. Not the same as
    /// the network `bye` of a runner or hub.
    Done {
        /// The job.
        job: JobId,
    },
    /// Watch a job on this connection.
    Wait {
        /// The job.
        job: JobId,
    },
    /// Cancel.
    Cancel(CancelRequest),
    /// Read state, history, log and the rest.
    Read(CliRead),
    /// List the known brokers.
    Brokers,
    /// `jobs --mine`: the caller's jobs this hub knows (same OS user and label `--as`).
    Jobs {
        /// Label `--as` of the caller.
        label: String,
        /// `--active`: only queued and running jobs.
        active: bool,
    },
}

/// Messages from a hub to its CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CliReply {
    /// The JobId of a placed order.
    Submitted {
        /// The job.
        job: JobId,
    },
    /// The order was refused.
    Rejected {
        /// A `rejected_*` code.
        code: ReasonCode,
        /// Text with the cause and a hint.
        detail: String,
    },
    /// Output of a watched job.
    Output {
        /// The job.
        job: JobId,
        /// Text.
        text: String,
    },
    /// A watched job ended.
    Exit {
        /// The job.
        job: JobId,
        /// Final state.
        state: JobState,
        /// Exit code.
        exit_code: Option<i32>,
        /// Reason code.
        reason: Option<ReasonCode>,
    },
    /// Jobs a cancel touched.
    Cancelled {
        /// The jobs.
        jobs: Vec<JobId>,
    },
    /// Answer to a read request.
    Read {
        /// Broker that answered.
        broker: String,
        /// The answer.
        reply: ReadReply,
    },
    /// The known brokers.
    Brokers {
        /// The list.
        brokers: Vec<BrokerStatus>,
    },
    /// Answer to `jobs --mine`.
    Jobs {
        /// The jobs, oldest first.
        jobs: Vec<JobSummary>,
    },
    /// A refusal.
    Error(ProtocolError),
}

/// Administrative commands of a broker; accepted only on local IPC from an OS user with rights
/// on the broker's state directory (spec 3.9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdminRequest {
    /// Check and apply `policy.toml`.
    PolicyReload,
    /// Check `policy.toml` without applying.
    PolicyCheck,
    /// Reread `logging.toml`.
    LoggingReload,
    /// Issue a join code.
    BrokerAdd {
        /// Client name.
        name: Option<String>,
        /// Client label.
        label: Option<String>,
        /// Number of joins the code serves.
        runners: Option<u32>,
        /// Address to print when several are listened on.
        address: Option<String>,
    },
    /// Remove a runner and revoke its key.
    RunnerRemove {
        /// Runner name.
        name: String,
    },
    /// Remove a client and revoke its key.
    ClientRemove {
        /// Client name.
        name: String,
    },
    /// List runners.
    RunnerList,
    /// List clients.
    ClientList,
}

/// Answers to administrative commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdminReply {
    /// Done, with warnings that do not stop it.
    Ok {
        /// Warnings, such as shadowed rules.
        warnings: Vec<String>,
    },
    /// A join code.
    JoinCode {
        /// Address to join.
        address: String,
        /// The one-time code.
        code: String,
        /// Certificate fingerprint of the broker.
        fingerprint: String,
        /// When the code expires.
        expires_at: Timestamp,
    },
    /// Names of runners or clients.
    Names {
        /// The names.
        names: Vec<String>,
    },
    /// Refused or failed.
    Failed {
        /// Why.
        message: String,
    },
}

/// How `down` stops a runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownMode {
    /// Finish the current job, stop services, say `bye` (the default).
    Drain,
    /// Cancel the job with `runner_stopped`, then `bye`.
    Cancel,
}

/// Commands on a runner's local control channel (spec 3.9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunnerControl {
    /// Stop the runner.
    Down {
        /// How.
        mode: DownMode,
    },
}

/// Answer of a runner's control channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlReply {
    /// Done.
    Ok,
    /// Failed.
    Failed {
        /// Why.
        message: String,
    },
}

message!(
    CliRequest,
    local: true,
    ["submit", "done", "wait", "cancel", "read", "brokers", "jobs"]
);
message!(
    CliReply,
    local: true,
    [
        "submitted",
        "rejected",
        "output",
        "exit",
        "cancelled",
        "read",
        "brokers",
        "jobs",
        "error"
    ]
);
message!(
    AdminRequest,
    local: true,
    [
        "policy_reload",
        "policy_check",
        "logging_reload",
        "broker_add",
        "runner_remove",
        "client_remove",
        "runner_list",
        "client_list"
    ]
);
message!(
    AdminReply,
    local: true,
    ["ok", "join_code", "names", "failed"]
);
message!(RunnerControl, local: true, ["down"]);
message!(IpcOpen, local: true, ["hello"]);
message!(IpcOpenReply, local: true, ["ready", "refused"]);
message!(ControlReply, local: true, ["ok", "failed"]);
