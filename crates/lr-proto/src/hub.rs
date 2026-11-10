use crate::api::{QueueEvent, ReadReply, ReadRequest};
use crate::caps::WireCapabilities;
use crate::common::{Ack, Bye, Hello, Join, Joined, ProtocolError, Resume, Welcome};
use crate::frame::message;
use crate::job::{
    Accepted, CancelAck, Chunk, HubCancel, JobResult, Order, Rejected, Returned, Started,
    StepResult,
};
use crate::seq::{final_result, seq_from_one, Direction, Sequenced};
use lr_core::{JobId, Zone};
use serde::{Deserialize, Serialize};

/// The hub asks which runners a broker has (spec 3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitiesQuery {
    /// Correlation id echoed in the reply.
    pub req: u64,
}

/// One runner in a capabilities reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerCaps {
    /// Runner name.
    pub name: String,
    /// Whether it is online.
    pub online: bool,
    /// What it offers.
    pub caps: WireCapabilities,
}

/// The broker's runners, with the zone relative to the asking hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitiesReply {
    /// Correlation id of the query.
    pub req: u64,
    /// Zone of the broker relative to the hub.
    pub zone: Zone,
    /// The runners.
    pub runners: Vec<RunnerCaps>,
}

/// A read request with its correlation id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadCall {
    /// Correlation id echoed in the answer.
    pub req: u64,
    /// What is asked.
    pub request: ReadRequest,
}

/// The answer to a read request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadAnswer {
    /// Correlation id of the request.
    pub req: u64,
    /// The answer.
    pub reply: ReadReply,
}

/// A piece of a followed log, for the request `req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogChunk {
    /// Correlation id of the log request.
    pub req: u64,
    /// Log text.
    pub text: String,
    /// No more text follows.
    pub end: bool,
}

/// An event of a subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventMsg {
    /// Subscription id.
    pub sub: u64,
    /// What happened.
    pub event: QueueEvent,
}

/// Messages a hub sends to a broker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubToBroker {
    /// Connect with an existing key.
    Hello(Hello),
    /// Join the broker with a one-time code.
    Join(Join),
    /// Ask for the runners and their capabilities.
    CapabilitiesQuery(CapabilitiesQuery),
    /// Place an order.
    Order(Order),
    /// Cancel a job.
    Cancel(HubCancel),
    /// Last accepted seq of broker messages per job after a reconnect.
    Resume(Resume),
    /// Confirms broker messages up to `seq`.
    Ack(Ack),
    /// Every 15 seconds.
    Heartbeat,
    /// The hub leaves in an orderly way.
    Bye(Bye),
    /// Read state, history, explain, log, stats or subscribe.
    Read(ReadCall),
    /// A refusal.
    Error(ProtocolError),
}

/// Messages a broker sends to a hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrokerToHub {
    /// Answer to `hello`, with the zone of the hub.
    Welcome(Welcome),
    /// Answer to `join`.
    Joined(Joined),
    /// Reply to the capabilities query.
    CapabilitiesReply(CapabilitiesReply),
    /// The order is stored.
    Accepted(Accepted),
    /// The order is refused.
    Rejected(Rejected),
    /// The job started.
    Started(Started),
    /// A piece of output.
    Chunk(Chunk),
    /// Result of a step.
    StepResult(StepResult),
    /// Final result of a job.
    JobResult(JobResult),
    /// A cancel is confirmed.
    CancelAck(CancelAck),
    /// An accepted order that did not start is given back.
    Returned(Returned),
    /// Answer to a read request.
    ReadReply(ReadAnswer),
    /// A piece of a followed log.
    LogChunk(LogChunk),
    /// An event of a subscription.
    Event(EventMsg),
    /// Last accepted seq of hub messages per job after a reconnect.
    Resume(Resume),
    /// Confirms hub messages up to `seq`.
    Ack(Ack),
    /// Every 15 seconds.
    Heartbeat,
    /// The broker closes in an orderly way.
    Bye(Bye),
    /// A refusal.
    Error(ProtocolError),
}

message!(
    HubToBroker,
    local: false,
    [
        "hello",
        "join",
        "capabilities_query",
        "order",
        "cancel",
        "resume",
        "ack",
        "heartbeat",
        "bye",
        "read",
        "error"
    ],
    check: |m: &HubToBroker| match m {
        HubToBroker::Order(o) => seq_from_one(o.seq),
        HubToBroker::Cancel(c) => seq_from_one(c.seq),
        _ => Ok(()),
    }
);

message!(
    BrokerToHub,
    local: false,
    [
        "welcome",
        "joined",
        "capabilities_reply",
        "accepted",
        "rejected",
        "started",
        "chunk",
        "step_result",
        "job_result",
        "cancel_ack",
        "returned",
        "read_reply",
        "log_chunk",
        "event",
        "resume",
        "ack",
        "heartbeat",
        "bye",
        "error"
    ],
    check: |m: &BrokerToHub| match m {
        BrokerToHub::Started(s) => seq_from_one(s.seq),
        BrokerToHub::Chunk(c) => seq_from_one(c.seq),
        BrokerToHub::StepResult(s) => seq_from_one(s.seq),
        BrokerToHub::JobResult(r) => final_result(r),
        BrokerToHub::CancelAck(c) => seq_from_one(c.seq),
        BrokerToHub::Returned(r) => seq_from_one(r.seq),
        BrokerToHub::Rejected(r) if !r.code.is_rejection() => {
            Err(format!("rejection with code `{}`", r.code))
        }
        _ => Ok(()),
    }
);

impl Sequenced for HubToBroker {
    const DIRECTION: Direction = Direction::HubToBroker;

    fn sequence(&self) -> Option<(&JobId, u64)> {
        match self {
            HubToBroker::Order(m) => Some((&m.job, m.seq)),
            HubToBroker::Cancel(m) => Some((&m.job, m.seq)),
            _ => None,
        }
    }
}

impl Sequenced for BrokerToHub {
    const DIRECTION: Direction = Direction::BrokerToHub;

    fn sequence(&self) -> Option<(&JobId, u64)> {
        match self {
            BrokerToHub::Started(m) => Some((&m.job, m.seq)),
            BrokerToHub::Chunk(m) => Some((&m.job, m.seq)),
            BrokerToHub::StepResult(m) => Some((&m.job, m.seq)),
            BrokerToHub::JobResult(m) => Some((&m.job, m.seq)),
            BrokerToHub::CancelAck(m) => Some((&m.job, m.seq)),
            BrokerToHub::Returned(m) => Some((&m.job, m.seq)),
            _ => None,
        }
    }
}
