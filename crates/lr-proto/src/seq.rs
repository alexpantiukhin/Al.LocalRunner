use crate::job::JobResult;
use lr_core::JobId;

/// One of the four message streams of a job; `seq` is counted separately for each (spec 3.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Direction {
    /// Hub to broker: orders and cancels.
    HubToBroker,
    /// Broker to hub: started, output, results, cancel confirmations.
    BrokerToHub,
    /// Broker to runner: assign and cancel.
    BrokerToRunner,
    /// Runner to broker: started, output, results.
    RunnerToBroker,
}

/// Messages that may carry a position in the stream of a job.
pub trait Sequenced {
    /// The stream the messages of this enum travel in.
    const DIRECTION: Direction;

    /// Job and seq, `None` for messages outside the per-job streams.
    fn sequence(&self) -> Option<(&JobId, u64)>;
}

/// The idempotency key of a message about a job: client key, job and seq, in one direction
/// (spec 3.9). A repeat after reconnect has an equal key and changes nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DedupKey {
    /// Key of the client (or of the runner's owner) on the connection.
    pub client: String,
    /// The job.
    pub job: JobId,
    /// Position in the stream.
    pub seq: u64,
    /// The stream.
    pub direction: Direction,
}

impl DedupKey {
    /// The key of `msg` received on a connection of `client`, `None` for a message without seq.
    pub fn of<M: Sequenced>(client: &str, msg: &M) -> Option<DedupKey> {
        let (job, seq) = msg.sequence()?;
        Some(DedupKey {
            client: client.to_string(),
            job: job.clone(),
            seq,
            direction: M::DIRECTION,
        })
    }
}

/// A per-broker outgoing counter of one job stream. The stream is bound to the broker that
/// accepted the order: the order is seq 1, and when it goes to another broker the stream
/// restarts at 1.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeqStream {
    last: u64,
}

impl SeqStream {
    /// A stream with nothing sent.
    pub fn new() -> SeqStream {
        SeqStream::default()
    }

    /// The next seq, starting at 1.
    pub fn next_seq(&mut self) -> u64 {
        self.last += 1;
        self.last
    }

    /// The last seq handed out, 0 when none.
    pub fn last(&self) -> u64 {
        self.last
    }

    /// Starts over for another broker.
    pub fn restart(&mut self) {
        self.last = 0;
    }
}

/// How an incoming seq relates to the last accepted one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqCheck {
    /// The next one: accept it.
    Next,
    /// Already accepted: drop it, idempotently.
    Repeat,
    /// Skips ahead: a protocol error.
    Gap {
        /// The seq that should have come.
        expected: u64,
    },
}

/// Classifies `seq` against the `last` accepted seq of the same stream.
pub fn classify_seq(last: u64, seq: u64) -> SeqCheck {
    if seq <= last {
        SeqCheck::Repeat
    } else if seq == last + 1 {
        SeqCheck::Next
    } else {
        SeqCheck::Gap { expected: last + 1 }
    }
}

pub(crate) fn seq_from_one(seq: u64) -> Result<(), String> {
    if seq == 0 {
        return Err("seq counts from 1".into());
    }
    Ok(())
}

pub(crate) fn final_result(r: &JobResult) -> Result<(), String> {
    seq_from_one(r.seq)?;
    if !r.state.is_final() {
        return Err(format!("result state {:?} is not final", r.state));
    }
    Ok(())
}
