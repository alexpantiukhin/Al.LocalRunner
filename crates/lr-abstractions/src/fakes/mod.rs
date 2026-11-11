//! In-memory fakes of every contract, for role tests (feature `test-support`). Each fake keeps
//! the invariants its trait documents, so a role that passes on the fakes relies only on what
//! the real implementations guarantee.

mod broker_jobs;
mod broker_store;
mod clock;
mod files;
mod host;
mod hub_runner_store;
mod misc;
mod outbox;
mod process;
mod repository;
mod transport;

pub use broker_store::MemoryBrokerStore;
pub use clock::ManualClock;
pub use files::MemoryFiles;
pub use host::{FakeDiskSpace, FakeKeepAwake};
pub use hub_runner_store::{MemoryHubStore, MemoryRunnerStore};
pub use misc::{FakeDataSource, MemoryLogSink, MemoryMeter, MemorySecrets, MeterUpdate};
pub use process::{
    FakeProcessInspector, FakeProcessRunner, JobScript, RecordingOutput, StepScript,
};
pub use repository::MemoryRepository;
pub use transport::{
    channel_pair, ChannelTransport, FakeConnection, FakeDialer, FakeListener, FakeNetwork, Faults,
};
