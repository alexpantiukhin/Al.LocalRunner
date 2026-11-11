use super::outbox::{accept_next, MemoryOutbox};
use crate::store::{
    ActiveOrder, BrokerLink, HubStore, OutboxBacklog, OutboxEntry, OutboxKind, RunnerStore,
    StoreError,
};
use crate::store::{CacheEntry, CacheKey, ServiceRow};
use async_trait::async_trait;
use lr_core::{JobId, Timestamp, Zone};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct HubState {
    active: BTreeMap<JobId, ActiveOrder>,
    outbox: MemoryOutbox,
    brokers: BTreeMap<String, BrokerLink>,
    files: BTreeMap<PathBuf, Vec<u8>>,
    failures: BTreeMap<&'static str, usize>,
    job_id_floor: u64,
}

impl HubState {
    fn injected(&mut self, op: &'static str) -> Result<(), StoreError> {
        match self.failures.get_mut(op) {
            Some(n) if *n > 0 => {
                *n -= 1;
                Err(StoreError::Io(format!("injected failure of {op}")))
            }
            _ => Ok(()),
        }
    }
}

/// `hub.db` and the log copies in memory, with the invariants of [`HubStore`].
#[derive(Default)]
pub struct MemoryHubStore {
    state: Arc<Mutex<HubState>>,
}

impl MemoryHubStore {
    /// An empty store.
    pub fn new() -> MemoryHubStore {
        MemoryHubStore::default()
    }

    /// Another handle over the same data, as if the database were closed and opened again.
    pub fn reopen(&self) -> MemoryHubStore {
        MemoryHubStore {
            state: self.state.clone(),
        }
    }

    /// The next `times` calls of the operation `op` (`enqueue`, `set_broker`) fail with
    /// [`StoreError::Io`] and change nothing.
    pub fn fail(&self, op: &'static str, times: usize) {
        self.state.lock().unwrap().failures.insert(op, times);
    }

    /// Content of a log copy written with `append_log_copy`.
    pub fn file(&self, path: &Path) -> Option<Vec<u8>> {
        self.state.lock().unwrap().files.get(path).cloned()
    }
}

fn missing(job: &JobId) -> StoreError {
    StoreError::NotFound(format!("active order {job}"))
}

#[async_trait]
impl HubStore for MemoryHubStore {
    async fn enqueue(
        &self,
        order: ActiveOrder,
        kind: OutboxKind,
        payload: String,
        at: Timestamp,
    ) -> Result<u64, StoreError> {
        let mut s = self.state.lock().unwrap();
        s.injected("enqueue")?;
        if s.active.contains_key(&order.job) {
            return Err(StoreError::Duplicate(format!("active order {}", order.job)));
        }
        let job = order.job.clone();
        s.active.insert(job.clone(), order);
        Ok(s.outbox.push("", &job, kind, payload, at))
    }

    async fn active(&self, job: &JobId) -> Result<Option<ActiveOrder>, StoreError> {
        Ok(self.state.lock().unwrap().active.get(job).cloned())
    }

    async fn active_orders(&self) -> Result<Vec<ActiveOrder>, StoreError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .active
            .values()
            .cloned()
            .collect())
    }

    async fn set_broker(&self, job: &JobId, broker: Option<&str>) -> Result<(), StoreError> {
        let mut s = self.state.lock().unwrap();
        s.injected("set_broker")?;
        let order = s.active.get_mut(job).ok_or_else(|| missing(job))?;
        order.broker = broker.map(str::to_string);
        Ok(())
    }

    async fn accept_seq(&self, job: &JobId, seq: u64) -> Result<bool, StoreError> {
        let mut s = self.state.lock().unwrap();
        let order = s.active.get_mut(job).ok_or_else(|| missing(job))?;
        accept_next(&mut order.last_seq, seq)
    }

    async fn finish(&self, job: &JobId) -> Result<(), StoreError> {
        let mut s = self.state.lock().unwrap();
        s.active.remove(job).ok_or_else(|| missing(job))?;
        s.outbox.discard("", job);
        Ok(())
    }

    async fn push_outbox(
        &self,
        job: &JobId,
        kind: OutboxKind,
        payload: String,
        at: Timestamp,
    ) -> Result<u64, StoreError> {
        let mut s = self.state.lock().unwrap();
        if !s.active.contains_key(job) {
            return Err(missing(job));
        }
        Ok(s.outbox.push("", job, kind, payload, at))
    }

    async fn pending_outbox(&self) -> Result<Vec<OutboxEntry>, StoreError> {
        Ok(self.state.lock().unwrap().outbox.pending(None))
    }

    async fn outbox_backlog(&self) -> Result<OutboxBacklog, StoreError> {
        Ok(OutboxBacklog::of(
            &self.state.lock().unwrap().outbox.pending(None),
        ))
    }

    async fn ack_outbox(&self, job: &JobId, seq: u64) -> Result<usize, StoreError> {
        Ok(self.state.lock().unwrap().outbox.ack("", job, seq))
    }

    async fn discard_outbox(&self, job: &JobId) -> Result<usize, StoreError> {
        Ok(self.state.lock().unwrap().outbox.discard("", job))
    }

    async fn rebroker(
        &self,
        job: &JobId,
        kind: OutboxKind,
        payload: String,
        at: Timestamp,
    ) -> Result<u64, StoreError> {
        let mut s = self.state.lock().unwrap();
        let order = s.active.get_mut(job).ok_or_else(|| missing(job))?;
        order.broker = None;
        order.last_seq = 0;
        s.outbox.restart("", job);
        Ok(s.outbox.push("", job, kind, payload, at))
    }

    async fn job_id_floor(&self) -> Result<u64, StoreError> {
        Ok(self.state.lock().unwrap().job_id_floor)
    }

    async fn raise_job_id_floor(&self, ms: u64) -> Result<(), StoreError> {
        let mut s = self.state.lock().unwrap();
        s.job_id_floor = s.job_id_floor.max(ms);
        Ok(())
    }

    async fn upsert_broker(&self, link: BrokerLink) -> Result<(), StoreError> {
        self.state
            .lock()
            .unwrap()
            .brokers
            .insert(link.name.clone(), link);
        Ok(())
    }

    async fn brokers(&self) -> Result<Vec<BrokerLink>, StoreError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .brokers
            .values()
            .cloned()
            .collect())
    }

    async fn remove_broker(&self, name: &str) -> Result<(), StoreError> {
        self.state
            .lock()
            .unwrap()
            .brokers
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| StoreError::NotFound(format!("broker {name}")))
    }

    async fn set_zone(&self, name: &str, zone: Zone) -> Result<(), StoreError> {
        let mut s = self.state.lock().unwrap();
        let link = s
            .brokers
            .get_mut(name)
            .ok_or_else(|| StoreError::NotFound(format!("broker {name}")))?;
        link.last_zone = Some(zone);
        Ok(())
    }

    async fn append_log_copy(&self, path: &Path, data: &[u8]) -> Result<(), StoreError> {
        self.state
            .lock()
            .unwrap()
            .files
            .entry(path.to_path_buf())
            .or_default()
            .extend_from_slice(data);
        Ok(())
    }
}

#[derive(Default)]
struct RunnerState {
    outbox: MemoryOutbox,
    services: BTreeMap<String, ServiceRow>,
    cache: BTreeMap<CacheKey, CacheEntry>,
    received: BTreeMap<JobId, u64>,
}

/// `runner.db` in memory, with the invariants of [`RunnerStore`].
#[derive(Default)]
pub struct MemoryRunnerStore {
    state: Arc<Mutex<RunnerState>>,
}

impl MemoryRunnerStore {
    /// An empty store.
    pub fn new() -> MemoryRunnerStore {
        MemoryRunnerStore::default()
    }

    /// Another handle over the same data, as if the database were closed and opened again.
    pub fn reopen(&self) -> MemoryRunnerStore {
        MemoryRunnerStore {
            state: self.state.clone(),
        }
    }
}

#[async_trait]
impl RunnerStore for MemoryRunnerStore {
    async fn push_outbox(
        &self,
        job: &JobId,
        kind: OutboxKind,
        payload: String,
        at: Timestamp,
    ) -> Result<u64, StoreError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .outbox
            .push("", job, kind, payload, at))
    }

    async fn pending_outbox(&self) -> Result<Vec<OutboxEntry>, StoreError> {
        Ok(self.state.lock().unwrap().outbox.pending(None))
    }

    async fn outbox_backlog(&self) -> Result<OutboxBacklog, StoreError> {
        Ok(OutboxBacklog::of(
            &self.state.lock().unwrap().outbox.pending(None),
        ))
    }

    async fn ack_outbox(&self, job: &JobId, seq: u64) -> Result<usize, StoreError> {
        Ok(self.state.lock().unwrap().outbox.ack("", job, seq))
    }

    async fn discard_outbox(&self, job: &JobId) -> Result<usize, StoreError> {
        Ok(self.state.lock().unwrap().outbox.discard("", job))
    }

    async fn clear_outbox(&self) -> Result<usize, StoreError> {
        Ok(self.state.lock().unwrap().outbox.remove(|_, _| true))
    }

    async fn accept_seq(&self, job: &JobId, seq: u64) -> Result<bool, StoreError> {
        let mut s = self.state.lock().unwrap();
        accept_next(s.received.entry(job.clone()).or_insert(0), seq)
    }

    async fn last_seq(&self, job: &JobId) -> Result<u64, StoreError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .received
            .get(job)
            .copied()
            .unwrap_or(0))
    }

    async fn forget_finished(&self, before: Timestamp) -> Result<usize, StoreError> {
        let mut s = self.state.lock().unwrap();
        let done = s.outbox.forget_finished(before);
        for job in &done {
            s.received.remove(job);
        }
        Ok(done.len())
    }

    async fn upsert_service(&self, service: ServiceRow) -> Result<(), StoreError> {
        self.state
            .lock()
            .unwrap()
            .services
            .insert(service.name.clone(), service);
        Ok(())
    }

    async fn services(&self) -> Result<Vec<ServiceRow>, StoreError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .services
            .values()
            .cloned()
            .collect())
    }

    async fn remove_service(&self, name: &str) -> Result<(), StoreError> {
        self.state
            .lock()
            .unwrap()
            .services
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| StoreError::NotFound(format!("service {name}")))
    }

    async fn upsert_cache(&self, entry: CacheEntry) -> Result<(), StoreError> {
        self.state
            .lock()
            .unwrap()
            .cache
            .insert(entry.key.clone(), entry);
        Ok(())
    }

    async fn cache_entries(&self) -> Result<Vec<CacheEntry>, StoreError> {
        Ok(self.state.lock().unwrap().cache.values().cloned().collect())
    }

    async fn remove_cache(&self, key: &CacheKey) -> Result<(), StoreError> {
        self.state
            .lock()
            .unwrap()
            .cache
            .remove(key)
            .map(|_| ())
            .ok_or_else(|| StoreError::NotFound(format!("cache {}/{}", key.project, key.name)))
    }
}
