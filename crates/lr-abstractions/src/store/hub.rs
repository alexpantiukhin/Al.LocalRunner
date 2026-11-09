use super::{OutboxEntry, OutboxKind, StoreError};
use async_trait::async_trait;
use lr_core::{JobId, Timestamp, Zone};
use std::path::Path;

/// Row of `active_order`: a job of this hub that has not reached its final state, with what
/// the reaper and the log copy need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveOrder {
    /// The job.
    pub job: JobId,
    /// Broker that took it; `None` while it waits in the outbox for a broker choice.
    pub broker: Option<String>,
    /// Greatest seq received from the broker for this job.
    pub last_seq: u64,
    /// Label `--as`.
    pub label: String,
    /// OS user of the submitter.
    pub os_user: String,
    /// Submitter process id; 0 when unknown.
    pub submitter_pid: u32,
    /// Submitter start fingerprint; `None` when unknown.
    pub submitter_start: Option<u64>,
    /// Handle file `temp/runner/<job-id>.json` in the submitter's directory.
    pub handle_path: Option<String>,
    /// `--detach`: the reaper leaves the job alone.
    pub detached: bool,
}

impl ActiveOrder {
    /// A job waiting for a broker choice, not detached, without handle.
    pub fn new(
        job: JobId,
        label: &str,
        os_user: &str,
        submitter_pid: u32,
        submitter_start: Option<u64>,
    ) -> ActiveOrder {
        ActiveOrder {
            job,
            broker: None,
            last_seq: 0,
            label: label.to_string(),
            os_user: os_user.to_string(),
            submitter_pid,
            submitter_start,
            handle_path: None,
            detached: false,
        }
    }
}

/// Row of `broker_link`: a broker this hub joined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerLink {
    /// Broker name in the hub's list and `hub-policy.toml`.
    pub name: String,
    /// `host:port`.
    pub address: String,
    /// Pinned certificate fingerprint.
    pub fingerprint: String,
    /// Identifier of this hub's key at that broker.
    pub key_id: String,
    /// MachineId of the broker.
    pub machine_id: String,
    /// Last zone the broker reported for this hub.
    pub last_zone: Option<Zone>,
}

/// `hub.db` of a hub and its log copies (spec 13.3, 14): the outbox of orders and cancels, the
/// active orders and the broker list.
///
/// Outbox entries belong to an active order; their seq counts per job from 1 and keeps
/// growing after acknowledgements, so a repeat is always recognisable.
#[async_trait]
pub trait HubStore: Send + Sync {
    /// Stores a new active order and its first outbox entry in one write; returns the seq.
    /// A job already active is `Duplicate`.
    async fn enqueue(
        &self,
        order: ActiveOrder,
        kind: OutboxKind,
        payload: String,
        at: Timestamp,
    ) -> Result<u64, StoreError>;

    /// One active order.
    async fn active(&self, job: &JobId) -> Result<Option<ActiveOrder>, StoreError>;

    /// All active orders, by job id.
    async fn active_orders(&self) -> Result<Vec<ActiveOrder>, StoreError>;

    /// Sets or clears the broker of an active order.
    async fn set_broker(&self, job: &JobId, broker: Option<&str>) -> Result<(), StoreError>;

    /// Records message `seq` received from the broker: `true` for the next seq
    /// (`last_seq + 1`, which it becomes), `false` for a repeat (`seq <= last_seq`),
    /// [`StoreError::SeqGap`] beyond the next. After [`HubStore::finish`] the order is gone
    /// and this returns `NotFound`, which the caller treats as a repeat of a finished job.
    async fn accept_seq(&self, job: &JobId, seq: u64) -> Result<bool, StoreError>;

    /// The job reached its final state: removes the active order and its outbox entries.
    async fn finish(&self, job: &JobId) -> Result<(), StoreError>;

    /// Stores an outgoing message of an active order with its next seq.
    async fn push_outbox(
        &self,
        job: &JobId,
        kind: OutboxKind,
        payload: String,
        at: Timestamp,
    ) -> Result<u64, StoreError>;

    /// Unacknowledged entries, oldest first, by seq within a job.
    async fn pending_outbox(&self) -> Result<Vec<OutboxEntry>, StoreError>;

    /// Removes the job's entries up to and including `seq` (`Accepted` or ack); returns how
    /// many.
    async fn ack_outbox(&self, job: &JobId, seq: u64) -> Result<usize, StoreError>;

    /// Removes every unacknowledged entry of the job (cancel before `Accepted`, outbox
    /// expiry); returns how many.
    async fn discard_outbox(&self, job: &JobId) -> Result<usize, StoreError>;

    /// Adds or replaces a broker.
    async fn upsert_broker(&self, link: BrokerLink) -> Result<(), StoreError>;

    /// All brokers, by name.
    async fn brokers(&self) -> Result<Vec<BrokerLink>, StoreError>;

    /// Forgets a broker.
    async fn remove_broker(&self, name: &str) -> Result<(), StoreError>;

    /// Records the zone the broker reported.
    async fn set_zone(&self, name: &str, zone: Zone) -> Result<(), StoreError>;

    /// Appends output to the log copy `temp/runner/<job-id>.log` at `path` in the submitter's
    /// directory, creating it and its directory.
    async fn append_log_copy(&self, path: &Path, data: &[u8]) -> Result<(), StoreError>;
}
