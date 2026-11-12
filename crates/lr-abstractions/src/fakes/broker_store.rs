use super::broker_jobs::BrokerState;
use crate::store::{
    compute_stats, Admission, Applied, BrokerStore, Inbound, Intake, JobFilter, JobKey, JobRecord,
    JobSpecRow, Join, JoinCodeRow, LogAppend, MachineRow, OrderRow, OutboxBacklog, OutboxEntry,
    OutboxKind, PruneLimits, PruneReport, Revoked, RunnerRow, RunnerState, Stats, StatsQuery,
    StepUpdate, StoreError, Transition,
};
use async_trait::async_trait;
use lr_core::{JobId, Timestamp};
use std::sync::{Arc, Mutex, MutexGuard};

/// `broker.db` and `logs/` in memory, with the invariants of [`BrokerStore`].
#[derive(Default)]
pub struct MemoryBrokerStore {
    state: Arc<Mutex<BrokerState>>,
}

impl MemoryBrokerStore {
    /// An empty store.
    pub fn new() -> MemoryBrokerStore {
        MemoryBrokerStore::default()
    }

    /// Another handle over the same data, as if the database were closed and opened again.
    pub fn reopen(&self) -> MemoryBrokerStore {
        MemoryBrokerStore {
            state: self.state.clone(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, BrokerState> {
        self.state.lock().unwrap()
    }
}

fn machine_missing(name: &str) -> StoreError {
    StoreError::NotFound(format!("machine {name}"))
}

impl BrokerState {
    fn redeem(&mut self, join: Join) -> Result<MachineRow, StoreError> {
        let code = self
            .codes
            .get(&join.code_hash)
            .ok_or(StoreError::CodeUnknown)?;
        if join.at >= code.expires_at {
            return Err(StoreError::CodeExpired);
        }
        if code.uses_left == 0 {
            return Err(StoreError::CodeUsedUp);
        }
        if code.kind != join.machine.kind {
            return Err(StoreError::CodeKindMismatch);
        }
        if code.name.as_ref().is_some_and(|n| *n != join.machine.name) {
            return Err(StoreError::CodeNameMismatch);
        }
        let mut machine = join.machine;
        if let Some(label) = &code.label {
            machine.label = label.clone();
        }
        let taken = self.machines.values().any(|m| {
            m.revoked_at.is_none() && (m.name == machine.name || m.public_key == machine.public_key)
        });
        if taken {
            return Err(StoreError::Duplicate(format!("machine {}", machine.name)));
        }
        let code = self.codes.get_mut(&join.code_hash).unwrap();
        code.uses_left -= 1;
        code.machine_name = Some(machine.name.clone());
        if code.uses_left == 0 {
            code.redeemed_at = Some(join.at);
        }
        self.machines.insert(machine.name.clone(), machine.clone());
        Ok(machine)
    }

    fn revoke(&mut self, name: &str, at: Timestamp) -> Result<Revoked, StoreError> {
        let machine = self
            .machines
            .get_mut(name)
            .ok_or_else(|| machine_missing(name))?;
        machine.revoked_at.get_or_insert(at);
        let mut runners = Vec::new();
        for runner in self.runners.values_mut() {
            if runner.machine_name == name && runner.state != RunnerState::Removed {
                runner.state = RunnerState::Removed;
                runners.push(runner.name.clone());
            }
        }
        let outbox_removed = self.results.remove(|r, _| r == name);
        Ok(Revoked {
            runners,
            outbox_removed,
        })
    }

    fn upsert_runner(&mut self, runner: RunnerRow) -> Result<(), StoreError> {
        if !self.machines.contains_key(&runner.machine_name) {
            return Err(machine_missing(&runner.machine_name));
        }
        if let Some(old) = self.runners.get(&runner.name) {
            if old.machine_name != runner.machine_name && old.state != RunnerState::Removed {
                return Err(StoreError::Duplicate(format!("runner {}", runner.name)));
            }
        }
        self.runners.insert(runner.name.clone(), runner);
        Ok(())
    }
}

#[async_trait]
impl BrokerStore for MemoryBrokerStore {
    async fn accept(&self, intake: Intake) -> Result<Admission, StoreError> {
        self.lock().accept(intake)
    }

    async fn orders(&self) -> Result<Vec<OrderRow>, StoreError> {
        Ok(self.lock().orders())
    }

    async fn grant(&self, key: &JobKey, runner: &str) -> Result<(), StoreError> {
        self.lock().grant(key, runner)
    }

    async fn requeue(&self, key: &JobKey) -> Result<(), StoreError> {
        self.lock().requeue(key)
    }

    async fn transition(&self, key: &JobKey, change: Transition) -> Result<Applied, StoreError> {
        self.lock().transition(key, change)
    }

    async fn record_cancel(
        &self,
        key: &JobKey,
        by: &str,
        reason: &str,
    ) -> Result<Applied, StoreError> {
        self.lock().record_cancel(key, by, reason)
    }

    async fn replace_spec(&self, key: &JobKey, spec: JobSpecRow) -> Result<Applied, StoreError> {
        self.lock().replace_spec(key, spec)
    }

    async fn update_step(&self, key: &JobKey, update: StepUpdate) -> Result<Applied, StoreError> {
        self.lock().update_step(key, update)
    }

    async fn accept_seq(&self, key: &JobKey, from: Inbound, seq: u64) -> Result<bool, StoreError> {
        self.lock().accept_seq(key, from, seq)
    }

    async fn last_seq(&self, key: &JobKey, from: Inbound) -> Result<u64, StoreError> {
        self.lock().last_seq(key, from)
    }

    async fn next_seq_to_runner(&self, key: &JobKey) -> Result<u64, StoreError> {
        self.lock().next_seq_to_runner(key)
    }

    async fn append_log(
        &self,
        key: &JobKey,
        data: &[u8],
        at: Timestamp,
        limit_bytes: u64,
    ) -> Result<LogAppend, StoreError> {
        self.lock().append_log(key, data, at, limit_bytes)
    }

    async fn read_log(&self, key: &JobKey) -> Result<Vec<u8>, StoreError> {
        self.lock().read_log(key)
    }

    async fn job(&self, key: &JobKey) -> Result<Option<JobRecord>, StoreError> {
        Ok(self.lock().runs.get(key).map(|r| r.record.clone()))
    }

    async fn jobs(&self, filter: &JobFilter) -> Result<Vec<JobRecord>, StoreError> {
        Ok(self.lock().jobs(filter))
    }

    async fn stats(&self, query: &StatsQuery) -> Result<Stats, StoreError> {
        let all = self.lock().jobs(&JobFilter::default());
        Ok(compute_stats(&all, query))
    }

    async fn prune(&self, limits: PruneLimits) -> Result<PruneReport, StoreError> {
        Ok(self.lock().prune(limits))
    }

    async fn issue_code(&self, code: JoinCodeRow) -> Result<(), StoreError> {
        let mut s = self.lock();
        s.codes
            .retain(|_, c| c.uses_left > 0 && c.expires_at > code.issued_at);
        if s.codes.contains_key(&code.code_hash) {
            return Err(StoreError::Duplicate("join code".into()));
        }
        s.codes.insert(code.code_hash.clone(), code);
        Ok(())
    }

    async fn join_code(&self, code_hash: &str) -> Result<Option<JoinCodeRow>, StoreError> {
        Ok(self.lock().codes.get(code_hash).cloned())
    }

    async fn join_codes(&self) -> Result<Vec<JoinCodeRow>, StoreError> {
        Ok(self.lock().codes.values().cloned().collect())
    }

    async fn redeem(&self, join: Join) -> Result<MachineRow, StoreError> {
        self.lock().redeem(join)
    }

    async fn machines(&self) -> Result<Vec<MachineRow>, StoreError> {
        Ok(self.lock().machines.values().cloned().collect())
    }

    async fn machine_by_key(&self, public_key: &str) -> Result<Option<MachineRow>, StoreError> {
        Ok(self
            .lock()
            .machines
            .values()
            .find(|m| m.public_key == public_key)
            .cloned())
    }

    async fn touch_machine(&self, name: &str, at: Timestamp) -> Result<(), StoreError> {
        let mut s = self.lock();
        let machine = s
            .machines
            .get_mut(name)
            .ok_or_else(|| machine_missing(name))?;
        machine.last_seen = Some(at);
        Ok(())
    }

    async fn revoke(&self, name: &str, at: Timestamp) -> Result<Revoked, StoreError> {
        self.lock().revoke(name, at)
    }

    async fn upsert_runner(&self, runner: RunnerRow) -> Result<(), StoreError> {
        self.lock().upsert_runner(runner)
    }

    async fn set_runner_state(
        &self,
        name: &str,
        state: RunnerState,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let mut s = self.lock();
        let runner = s
            .runners
            .get_mut(name)
            .ok_or_else(|| StoreError::NotFound(format!("runner {name}")))?;
        runner.state = state;
        runner.last_seen = Some(at);
        Ok(())
    }

    async fn runners(&self) -> Result<Vec<RunnerRow>, StoreError> {
        Ok(self.lock().runners.values().cloned().collect())
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
        let mut s = self.lock();
        let over = s.results.bytes_for(client) + payload.len() as u64 > limit_bytes;
        if over && kind == OutboxKind::Chunk {
            return Ok(None);
        }
        Ok(Some(s.results.push(client, job, kind, payload, at)))
    }

    async fn pending_results(&self, client: &str) -> Result<Vec<OutboxEntry>, StoreError> {
        Ok(self.lock().results.pending(Some(client)))
    }

    async fn ack_results(&self, client: &str, job: &JobId, seq: u64) -> Result<usize, StoreError> {
        Ok(self.lock().results.ack(client, job, seq))
    }

    async fn expire_results(&self, before: Timestamp) -> Result<usize, StoreError> {
        Ok(self.lock().results.remove(|_, e| e.created_at < before))
    }

    async fn results_backlog(&self) -> Result<OutboxBacklog, StoreError> {
        Ok(OutboxBacklog::of(&self.lock().results.pending(None)))
    }
}
