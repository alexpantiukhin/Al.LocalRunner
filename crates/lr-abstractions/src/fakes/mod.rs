//! In-memory fakes of every contract, for role tests (feature `test-support`). Each fake keeps
//! the invariants its trait documents, so a role that passes on the fakes relies only on what
//! the real implementations guarantee.

mod broker_jobs;
mod broker_store;
mod clock;
mod hub_runner_store;
mod misc;
mod outbox;
mod process;
mod transport;

pub use broker_store::MemoryBrokerStore;
pub use clock::ManualClock;
pub use hub_runner_store::{MemoryHubStore, MemoryRunnerStore};
pub use misc::{FakeDataSource, MemoryLogSink, MemorySecrets};
pub use process::{
    FakeProcessInspector, FakeProcessRunner, JobScript, RecordingOutput, StepScript,
};
pub use transport::{
    channel_pair, ChannelTransport, FakeDialer, FakeListener, FakeNetwork, Faults,
};
