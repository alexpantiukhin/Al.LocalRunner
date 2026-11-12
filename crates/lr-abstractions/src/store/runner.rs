use super::{OutboxBacklog, OutboxEntry, OutboxKind, StoreError};
use async_trait::async_trait;
use lr_core::{JobId, Timestamp};

/// State of a runner service (spec 15.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
}

/// Row of `service_registry`: a live service, so orphans of a previous run are found by pid
/// and start fingerprint and killed at start (spec 3.11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRow {
    /// Service name.
    pub name: String,
    /// Process id of the service's root process.
    pub pid: u32,
    /// Its start fingerprint, compared before killing.
    pub pid_start: Option<u64>,
    /// State.
    pub state: ServiceState,
    /// When it started.
    pub started_at: Timestamp,
    /// `started_at + max_lifetime_s`.
    pub expires_at: Timestamp,
    /// Memory counted in the pool budget.
    pub mem_mb: u64,
}

/// Key of a cache directory: client key, project, cache name (spec 3.10).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CacheKey {
    /// Client key, or the shared marker when `[cache] share = "project"`.
    pub client: String,
    /// Project.
    pub project: String,
    /// Cache name from the profile.
    pub name: String,
}

/// Row of `cache_index`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    /// Key.
    pub key: CacheKey,
    /// Directory.
    pub path: String,
    /// Size in bytes.
    pub bytes: u64,
    /// Last use, for retention and LRU.
    pub last_used: Timestamp,
}

/// `runner.db` of a runner (spec 14): its outbox of output and results, the service registry
/// and the cache index. The outbox follows the same seq rules as the hub's.
#[async_trait]
pub trait RunnerStore: Send + Sync {
    /// Stores an outgoing message with the next seq of the job; returns it.
    async fn push_outbox(
        &self,
        job: &JobId,
        kind: OutboxKind,
        payload: String,
        at: Timestamp,
    ) -> Result<u64, StoreError>;

    /// Unacknowledged entries, oldest first, by seq within a job.
    async fn pending_outbox(&self) -> Result<Vec<OutboxEntry>, StoreError>;

    /// How many entries are unacknowledged and since when, without reading their payloads.
    async fn outbox_backlog(&self) -> Result<OutboxBacklog, StoreError>;

    /// Removes the job's entries up to and including `seq`; returns how many.
    async fn ack_outbox(&self, job: &JobId, seq: u64) -> Result<usize, StoreError>;

    /// Removes every entry of the job; returns how many.
    async fn discard_outbox(&self, job: &JobId) -> Result<usize, StoreError>;

    /// Removes every entry (the broker removed this runner); returns how many.
    async fn clear_outbox(&self) -> Result<usize, StoreError>;

    /// Records message `seq` of `job` received from the broker (assign, cancel): `true` for
    /// the next seq (`last + 1`), `false` for a repeat, [`StoreError::SeqGap`] beyond the
    /// next. Unknown jobs start at 0.
    async fn accept_seq(&self, job: &JobId, seq: u64) -> Result<bool, StoreError>;

    /// The last seq of `job` accepted from the broker, 0 when none; sent in `resume`.
    async fn last_seq(&self, job: &JobId) -> Result<u64, StoreError>;

    /// Forgets the jobs that are done: no entry is pending, the last entry pushed is a
    /// [`OutboxKind::JobResult`] created before `before`. Their closed entries, seq counters
    /// and received seq are removed so `runner.db` does not grow with every job; returns how
    /// many jobs were forgotten. A job that is forgotten and then appears again counts from
    /// seq 1.
    async fn forget_finished(&self, before: Timestamp) -> Result<usize, StoreError>;

    /// Adds or replaces a service by name.
    async fn upsert_service(&self, service: ServiceRow) -> Result<(), StoreError>;

    /// All recorded services, by name.
    async fn services(&self) -> Result<Vec<ServiceRow>, StoreError>;

    /// Forgets a service.
    async fn remove_service(&self, name: &str) -> Result<(), StoreError>;

    /// Adds or replaces a cache entry by key.
    async fn upsert_cache(&self, entry: CacheEntry) -> Result<(), StoreError>;

    /// All cache entries, by key.
    async fn cache_entries(&self) -> Result<Vec<CacheEntry>, StoreError>;

    /// Forgets a cache entry.
    async fn remove_cache(&self, key: &CacheKey) -> Result<(), StoreError>;
}
