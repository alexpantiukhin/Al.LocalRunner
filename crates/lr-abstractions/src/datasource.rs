use async_trait::async_trait;
use lr_core::JobId;
use std::fmt;

/// A stream of live events; `None` once the source is gone.
#[async_trait]
pub trait EventStream<E>: Send {
    /// The next event.
    async fn next(&mut self) -> Option<E>;
}

/// A cancel requested from the dashboard; it counts as `--force` and is recorded as
/// `dashboard:<OS user>` with the reason from the dialog (spec 9.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelRequest {
    /// Who cancels, for example `dashboard:alex`.
    pub actor: String,
    /// Reason typed in the dialog.
    pub reason: Option<String>,
}

/// Why the data source could not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataError {
    /// No such job, or it is not visible to this client.
    NotFound,
    /// The data cannot be read now (all brokers unreachable).
    Unavailable(String),
    /// The broker refused the action (not this client's order).
    Refused(String),
}

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataError::NotFound => f.write_str("not found"),
            DataError::Unavailable(r) => write!(f, "unavailable: {r}"),
            DataError::Refused(r) => write!(f, "refused: {r}"),
        }
    }
}

impl std::error::Error for DataError {}

/// The one interface the dashboard server and page read through (spec 9.3), implemented by
/// the hub from the broker APIs, its outbox and its log copies.
///
/// The view types are associated types so this crate does not define the API views, which
/// belong to the protocol crate: the snapshot carries everything the page shows (queue,
/// running jobs and steps, runners, history, stats, known brokers with stale marks, orders in
/// the outbox) and events carry its live updates. Cancel is the only action (spec 9.4).
#[async_trait]
pub trait DataSource: Send + Sync {
    /// Everything the page shows at this moment.
    type Snapshot: Send;
    /// One live update.
    type Event: Send;

    /// A fresh snapshot.
    async fn snapshot(&self) -> Result<Self::Snapshot, DataError>;

    /// Live updates from now on, for one subscriber (one SSE connection).
    async fn subscribe(&self) -> Result<Box<dyn EventStream<Self::Event>>, DataError>;

    /// The saved log of a job (from the broker, else the hub's copy).
    async fn log(&self, job: &JobId) -> Result<Vec<u8>, DataError>;

    /// Cancels a job of this hub's key.
    async fn cancel(&self, job: &JobId, request: CancelRequest) -> Result<(), DataError>;
}
