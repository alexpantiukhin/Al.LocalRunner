use async_trait::async_trait;
use lr_abstractions::fakes::MemoryBrokerStore;
use lr_abstractions::{
    Admission, Applied, BrokerStore, Inbound, Intake, JobFilter, JobKey, JobRecord, Join,
    JoinCodeRow, LogAppend, MachineRow, OrderRow, OutboxEntry, OutboxKind, PruneLimits,
    PruneReport, Revoked, RunnerRow, RunnerState, Stats, StatsQuery, StepUpdate, StoreError,
    Transition,
};
use lr_core::{JobId, Timestamp};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub struct FaultyStore {
    pub inner: Arc<MemoryBrokerStore>,
    pub fail_accept: AtomicBool,
    pub fail_accept_seq: AtomicBool,
    pub whole_log_reads: AtomicUsize,
    pub filters: Mutex<Vec<JobFilter>>,
}

impl FaultyStore {
    pub fn new(inner: Arc<MemoryBrokerStore>) -> FaultyStore {
        FaultyStore {
            inner,
            fail_accept: AtomicBool::new(false),
            fail_accept_seq: AtomicBool::new(false),
            whole_log_reads: AtomicUsize::new(0),
            filters: Mutex::new(Vec::new()),
        }
    }

    pub fn set(flag: &AtomicBool) {
        flag.store(true, Ordering::SeqCst);
    }

    pub fn reads(&self) -> usize {
        self.whole_log_reads.load(Ordering::SeqCst)
    }

    pub fn seen_filters(&self) -> Vec<JobFilter> {
        self.filters.lock().unwrap().clone()
    }
}

fn io() -> StoreError {
    StoreError::Io("disk full".into())
}

#[async_trait]
impl BrokerStore for FaultyStore {
    async fn accept(&self, intake: Intake) -> Result<Admission, StoreError> {
        if self.fail_accept.load(Ordering::SeqCst) {
            return Err(io());
        }
        self.inner.accept(intake).await
    }
    async fn orders(&self) -> Result<Vec<OrderRow>, StoreError> {
        self.inner.orders().await
    }
    async fn grant(&self, key: &JobKey, runner: &str) -> Result<(), StoreError> {
        self.inner.grant(key, runner).await
    }
    async fn requeue(&self, key: &JobKey) -> Result<(), StoreError> {
        self.inner.requeue(key).await
    }
    async fn transition(&self, key: &JobKey, change: Transition) -> Result<Applied, StoreError> {
        self.inner.transition(key, change).await
    }
    async fn update_step(&self, key: &JobKey, update: StepUpdate) -> Result<Applied, StoreError> {
        self.inner.update_step(key, update).await
    }
    async fn accept_seq(&self, key: &JobKey, from: Inbound, seq: u64) -> Result<bool, StoreError> {
        if self.fail_accept_seq.load(Ordering::SeqCst) {
            return Err(io());
        }
        self.inner.accept_seq(key, from, seq).await
    }
    async fn last_seq(&self, key: &JobKey, from: Inbound) -> Result<u64, StoreError> {
        self.inner.last_seq(key, from).await
    }
    async fn next_seq_to_runner(&self, key: &JobKey) -> Result<u64, StoreError> {
        self.inner.next_seq_to_runner(key).await
    }
    async fn append_log(
        &self,
        key: &JobKey,
        data: &[u8],
        at: Timestamp,
        limit_bytes: u64,
    ) -> Result<LogAppend, StoreError> {
        self.inner.append_log(key, data, at, limit_bytes).await
    }
    async fn read_log(&self, key: &JobKey) -> Result<Vec<u8>, StoreError> {
        self.whole_log_reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read_log(key).await
    }
    async fn read_log_tail(&self, key: &JobKey, max_bytes: u64) -> Result<Vec<u8>, StoreError> {
        self.inner.read_log_tail(key, max_bytes).await
    }
    async fn job(&self, key: &JobKey) -> Result<Option<JobRecord>, StoreError> {
        self.inner.job(key).await
    }
    async fn jobs(&self, filter: &JobFilter) -> Result<Vec<JobRecord>, StoreError> {
        self.filters.lock().unwrap().push(filter.clone());
        self.inner.jobs(filter).await
    }
    async fn stats(&self, query: &StatsQuery) -> Result<Stats, StoreError> {
        self.inner.stats(query).await
    }
    async fn prune(&self, limits: PruneLimits) -> Result<PruneReport, StoreError> {
        self.inner.prune(limits).await
    }
    async fn issue_code(&self, code: JoinCodeRow) -> Result<(), StoreError> {
        self.inner.issue_code(code).await
    }
    async fn join_code(&self, code_hash: &str) -> Result<Option<JoinCodeRow>, StoreError> {
        self.inner.join_code(code_hash).await
    }
    async fn join_codes(&self) -> Result<Vec<JoinCodeRow>, StoreError> {
        self.inner.join_codes().await
    }
    async fn redeem(&self, join: Join) -> Result<MachineRow, StoreError> {
        self.inner.redeem(join).await
    }
    async fn machines(&self) -> Result<Vec<MachineRow>, StoreError> {
        self.inner.machines().await
    }
    async fn machine_by_key(&self, public_key: &str) -> Result<Option<MachineRow>, StoreError> {
        self.inner.machine_by_key(public_key).await
    }
    async fn touch_machine(&self, name: &str, at: Timestamp) -> Result<(), StoreError> {
        self.inner.touch_machine(name, at).await
    }
    async fn revoke(&self, name: &str, at: Timestamp) -> Result<Revoked, StoreError> {
        self.inner.revoke(name, at).await
    }
    async fn upsert_runner(&self, runner: RunnerRow) -> Result<(), StoreError> {
        self.inner.upsert_runner(runner).await
    }
    async fn set_runner_state(
        &self,
        name: &str,
        state: RunnerState,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        self.inner.set_runner_state(name, state, at).await
    }
    async fn runners(&self) -> Result<Vec<RunnerRow>, StoreError> {
        self.inner.runners().await
    }
    async fn push_result(
        &self,
        client: &str,
        job: &JobId,
        kind: OutboxKind,
        payload: String,
        at: Timestamp,
        limit_bytes: u64,
    ) -> Result<Option<u64>, StoreError> {
        self.inner
            .push_result(client, job, kind, payload, at, limit_bytes)
            .await
    }
    async fn pending_results(&self, client: &str) -> Result<Vec<OutboxEntry>, StoreError> {
        self.inner.pending_results(client).await
    }
    async fn ack_results(&self, client: &str, job: &JobId, seq: u64) -> Result<usize, StoreError> {
        self.inner.ack_results(client, job, seq).await
    }
    async fn expire_results(&self, before: Timestamp) -> Result<usize, StoreError> {
        self.inner.expire_results(before).await
    }
}
