//! I/O contracts of the roles (spec 13.1, layer 3): time, storage, files, network, processes,
//! secrets, diagnostic sinks and the dashboard data source.
//!
//! Roles receive implementations of these traits through their constructors and never know
//! what stands behind them, so the broker engine, the hub reaper and the runner agent are
//! tested on the fakes of this crate (feature `test-support`) without SQLite, TLS, real
//! processes or waiting for real time.
//!
//! Async style, chosen once for the whole crate: async methods use the `async-trait` crate
//! (MIT/Apache-2.0), so every trait is object safe and roles hold `Arc<dyn Trait>`.
//! [`Clock::sleep`] is the one exception: it returns a boxed `'static` future directly, so the
//! deadline is fixed when `sleep` is called rather than when the future is first polled.
//!
//! The default build contains the traits and their data types only; the fakes and the
//! reusable store contract tests ([`store_contract`]) exist only with `test-support`, which
//! roles enable in `dev-dependencies`.

pub mod clock;
pub mod datasource;
pub mod files;
pub mod logsink;
pub mod process;
pub mod secrets;
pub mod store;
pub mod transport;

#[cfg(feature = "test-support")]
pub mod fakes;
#[cfg(feature = "test-support")]
pub mod store_contract;

pub use clock::{Clock, Sleep};
pub use datasource::{CancelRequest, DataError, DataSource, EventStream};
pub use files::{Files, FilesError};
pub use logsink::{Level, LogSink, Record, Role};
pub use process::{
    mask_secrets, stop_signal, JobEvent, JobOutcome, JobOutput, JobSpec, JobStep, Liveness,
    ProcessError, ProcessInspector, ProcessRef, ProcessRunner, SpawnSpec, StepOutcome, Stop,
    StopHandle, StopSignal, DEFAULT_CANCEL_GRACE, MASK,
};
pub use secrets::{SecretError, SecretSource, SecretValue};
pub use store::{
    outlives_cancel, ActiveOrder, Admission, Applied, BrokerLink, BrokerStore, CacheEntry,
    CacheKey, ClientUsage, CodeSource, GithubRunRow, HubStore, Inbound, Intake, JobFilter, JobKey,
    JobLogRow, JobRecord, JobRow, JobSpecRow, Join, JoinCodeRow, LogAppend, MachineKind,
    MachineRow, OrderRow, OutboxEntry, OutboxKind, ProfileStats, PruneLimits, PruneReport, Revoked,
    RunnerRow, RunnerState, RunnerStore, ServiceRow, ServiceState, SourceKind, Spread, Stats,
    StatsQuery, StepRow, StepStats, StepUpdate, StoreError, Transition, LOG_TRUNCATED_MARK,
};
pub use transport::{
    Dialer, Frame, Listener, Peer, PeerAccess, Transport, TransportError, MAX_FRAME_BYTES,
};
