use super::broker_rows::{
    Admission, Applied, Intake, JobKey, JobRecord, Join, JoinCodeRow, LogAppend, MachineRow,
    OrderRow, PruneLimits, PruneReport, Revoked, RunnerRow, RunnerState, StepUpdate, Transition,
};
use super::query::{JobFilter, Stats, StatsQuery};
use super::{OutboxEntry, OutboxKind, StoreError};
use async_trait::async_trait;
use lr_core::{JobId, Timestamp};

/// `broker.db` and `logs/` of a broker (spec 8, 14): run history with the client, the order
/// queue, the machine registry and join codes, runners, and the per-client result outbox.
///
/// Invariants every implementation keeps:
/// - a run is stored once per client key and `JobId`; a repeated order changes nothing;
/// - the final state of a run is written once; a later result or a second final state is
///   ignored and reported as [`Applied::Ignored`];
/// - an `order` row exists only before grant and is removed by the grant or by a cancel of the
///   waiting order;
/// - pruning never touches a run that is not final.
#[async_trait]
pub trait BrokerStore: Send + Sync {
    /// Stores an accepted order with its run, steps and spec, or reports a duplicate of the
    /// same client key and `JobId` without changing anything.
    async fn accept(&self, intake: Intake) -> Result<Admission, StoreError>;

    /// The order queue (rows before grant), oldest `enqueued_at` first.
    async fn orders(&self) -> Result<Vec<OrderRow>, StoreError>;

    /// Grants a waiting order to `runner`: the `order` row is removed and the run records the
    /// runner. Fails with `NotFound` when no order row exists.
    async fn grant(&self, key: &JobKey, runner: &str) -> Result<(), StoreError>;

    /// Puts a granted but not started run back into the queue (its runner vanished before
    /// start, or broker restart): the order row is stored again and the runner cleared.
    async fn requeue(&self, order: OrderRow) -> Result<(), StoreError>;

    /// Changes the state of a run as `lr_core::JobState::can_become` allows. `Running` needs
    /// a grant and sets `started_at`; a final state sets `finished_at` and the exit code and
    /// removes a waiting order row. Canceller and reason are kept once set. A change of a
    /// final run is [`Applied::Ignored`].
    async fn transition(&self, key: &JobKey, change: Transition) -> Result<Applied, StoreError>;

    /// Updates one step; ignored when the run is final or the step already ended.
    async fn update_step(&self, key: &JobKey, update: StepUpdate) -> Result<Applied, StoreError>;

    /// Records message `seq` received from the runner for this run: `true` when it is new
    /// (greater than every seq seen), `false` for a repeat that must be dropped (spec 3.9).
    async fn accept_seq(&self, key: &JobKey, seq: u64) -> Result<bool, StoreError>;

    /// The greatest seq accepted for this run, 0 when none; sent in `resume`.
    async fn last_seq(&self, key: &JobKey) -> Result<u64, StoreError>;

    /// Appends output to the run's log, keeping it within `limit_bytes` (spec 8).
    async fn append_log(
        &self,
        key: &JobKey,
        data: &[u8],
        at: Timestamp,
        limit_bytes: u64,
    ) -> Result<LogAppend, StoreError>;

    /// The run's stored log; empty when none was written or it was pruned.
    async fn read_log(&self, key: &JobKey) -> Result<Vec<u8>, StoreError>;

    /// One run with all its rows.
    async fn job(&self, key: &JobKey) -> Result<Option<JobRecord>, StoreError>;

    /// Runs matching `filter`, newest `queued_at` first.
    async fn jobs(&self, filter: &JobFilter) -> Result<Vec<JobRecord>, StoreError>;

    /// Statistics of final runs, as [`super::compute_stats`] defines them.
    async fn stats(&self, query: &StatsQuery) -> Result<Stats, StoreError>;

    /// Removes old logs and rows of final runs, oldest first.
    async fn prune(&self, limits: PruneLimits) -> Result<PruneReport, StoreError>;

    /// Stores a newly issued join code.
    async fn issue_code(&self, code: JoinCodeRow) -> Result<(), StoreError>;

    /// All join codes.
    async fn join_codes(&self) -> Result<Vec<JoinCodeRow>, StoreError>;

    /// Records a join and spends one use of the code in one write, so an interrupted join
    /// costs no use (spec 3.8). Fails without any change when the code is unknown, expired,
    /// used up or of another kind, or the name belongs to an unrevoked machine; a revoked
    /// machine of the same name is replaced.
    async fn redeem(&self, join: Join) -> Result<MachineRow, StoreError>;

    /// All key records, revoked ones included.
    async fn machines(&self) -> Result<Vec<MachineRow>, StoreError>;

    /// The key record holding `public_key`, revoked or not.
    async fn machine_by_key(&self, public_key: &str) -> Result<Option<MachineRow>, StoreError>;

    /// Sets `last_seen` of a machine.
    async fn touch_machine(&self, name: &str, at: Timestamp) -> Result<(), StoreError>;

    /// Revokes a key: sets `revoked_at`, marks its runners `Removed` and deletes its result
    /// outbox (spec 3.8). Waiting orders are cancelled by the engine.
    async fn revoke(&self, name: &str, at: Timestamp) -> Result<Revoked, StoreError>;

    /// Registers or updates a runner; a name held by another machine's active runner is
    /// `Duplicate`.
    async fn upsert_runner(&self, runner: RunnerRow) -> Result<(), StoreError>;

    /// Sets the state of a runner and its `last_seen`.
    async fn set_runner_state(
        &self,
        name: &str,
        state: RunnerState,
        at: Timestamp,
    ) -> Result<(), StoreError>;

    /// All runners.
    async fn runners(&self) -> Result<Vec<RunnerRow>, StoreError>;

    /// Stores a message for `client` with the next seq of (client, job). When the client's
    /// stored payloads would exceed `limit_bytes`, a [`OutboxKind::Chunk`] is dropped and
    /// `None` returned, while other kinds are always kept (spec 8).
    async fn push_result(
        &self,
        client: &str,
        job: &JobId,
        kind: OutboxKind,
        payload: String,
        at: Timestamp,
        limit_bytes: u64,
    ) -> Result<Option<u64>, StoreError>;

    /// Unacknowledged messages for `client`, oldest job first, by seq within a job.
    async fn pending_results(&self, client: &str) -> Result<Vec<OutboxEntry>, StoreError>;

    /// Removes the client's entries of `job` up to and including `seq`; returns how many.
    async fn ack_results(&self, client: &str, job: &JobId, seq: u64) -> Result<usize, StoreError>;

    /// Removes entries created before `before` (`[results] keep_hours`); returns how many.
    async fn expire_results(&self, before: Timestamp) -> Result<usize, StoreError>;
}
