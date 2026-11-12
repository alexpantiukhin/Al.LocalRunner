use crate::common::{
    Ack, Bye, CapabilitiesUpdate, Hello, Join, Joined, ProtocolError, Resume, ServicesReport,
    SettingsUpdate, StopService, Welcome,
};
use crate::frame::message;
use crate::job::{check_chunk, Assign, Chunk, JobResult, RunnerCancel, Started, StepResult};
use crate::seq::{final_result, seq_from_one, Direction, Sequenced};
use lr_core::JobId;
use serde::{Deserialize, Serialize};

/// Messages a runner sends to its broker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunnerToBroker {
    /// Connect with an existing key.
    Hello(Hello),
    /// Join the pool with a one-time code.
    Join(Join),
    /// Last accepted seq of broker messages per job after a reconnect.
    Resume(Resume),
    /// Confirms broker messages up to `seq`.
    Ack(Ack),
    /// Every 15 seconds.
    Heartbeat,
    /// Take no new orders, finish the current job.
    Drain,
    /// The runner leaves in an orderly way.
    Bye(Bye),
    /// The job started.
    Started(Started),
    /// A piece of output.
    Chunk(Chunk),
    /// Result of a step.
    StepResult(StepResult),
    /// Final result of a job.
    JobResult(JobResult),
    /// State of the runner's services.
    ServicesReport(ServicesReport),
    /// Changed capabilities or cache fill.
    CapabilitiesUpdate(CapabilitiesUpdate),
    /// A refusal.
    Error(ProtocolError),
}

/// Messages a broker sends to a runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrokerToRunner {
    /// Answer to `hello`.
    Welcome(Welcome),
    /// Answer to `join`.
    Joined(Joined),
    /// Last accepted seq of runner messages per job after a reconnect.
    Resume(Resume),
    /// Confirms runner messages up to `seq`.
    Ack(Ack),
    /// Every 15 seconds.
    Heartbeat,
    /// The broker closes in an orderly way.
    Bye(Bye),
    /// Run this job.
    Assign(Assign),
    /// Stop this job.
    Cancel(RunnerCancel),
    /// Stop a warm service.
    StopService(StopService),
    /// A changed `[grace]` for a connected runner.
    SettingsUpdate(SettingsUpdate),
    /// A refusal; an authorization failure is a command to kill the job tree.
    Error(ProtocolError),
}

message!(
    RunnerToBroker,
    local: false,
    [
        "hello",
        "join",
        "resume",
        "ack",
        "heartbeat",
        "drain",
        "bye",
        "started",
        "chunk",
        "step_result",
        "job_result",
        "services_report",
        "capabilities_update",
        "error"
    ],
    check: |m: &RunnerToBroker| match m {
        RunnerToBroker::Started(s) => seq_from_one(s.seq),
        RunnerToBroker::Chunk(c) => check_chunk(c),
        RunnerToBroker::StepResult(s) => seq_from_one(s.seq),
        RunnerToBroker::JobResult(r) => final_result(r),
        _ => Ok(()),
    }
);

message!(
    BrokerToRunner,
    local: false,
    [
        "welcome",
        "joined",
        "resume",
        "ack",
        "heartbeat",
        "bye",
        "assign",
        "cancel",
        "stop_service",
        "settings_update",
        "error"
    ],
    check: |m: &BrokerToRunner| match m {
        BrokerToRunner::Assign(a) => seq_from_one(a.seq),
        BrokerToRunner::Cancel(c) => seq_from_one(c.seq),
        _ => Ok(()),
    }
);

impl Sequenced for RunnerToBroker {
    const DIRECTION: Direction = Direction::RunnerToBroker;

    fn sequence(&self) -> Option<(&JobId, u64)> {
        match self {
            RunnerToBroker::Started(m) => Some((&m.job, m.seq)),
            RunnerToBroker::Chunk(m) => Some((&m.job, m.seq)),
            RunnerToBroker::StepResult(m) => Some((&m.job, m.seq)),
            RunnerToBroker::JobResult(m) => Some((&m.job, m.seq)),
            _ => None,
        }
    }
}

impl Sequenced for BrokerToRunner {
    const DIRECTION: Direction = Direction::BrokerToRunner;

    fn sequence(&self) -> Option<(&JobId, u64)> {
        match self {
            BrokerToRunner::Assign(m) => Some((&m.job, m.seq)),
            BrokerToRunner::Cancel(m) => Some((&m.job, m.seq)),
            _ => None,
        }
    }
}
