use super::outbox::{accept_next, MemoryOutbox};
use crate::store::{
    Admission, Applied, Inbound, Intake, JobFilter, JobKey, JobLogRow, JobRecord, JoinCodeRow,
    LogAppend, MachineRow, OrderRow, PruneLimits, PruneReport, RunnerRow, StepUpdate, StoreError,
    Transition, LOG_TRUNCATED_MARK,
};
use lr_core::{JobState, StepState, Timestamp};
use std::collections::BTreeMap;

pub(crate) struct Run {
    pub(crate) record: JobRecord,
    log: Vec<u8>,
    truncated: bool,
    order: OrderRow,
    received: BTreeMap<Inbound, u64>,
    to_runner: u64,
}

#[derive(Default)]
pub(crate) struct BrokerState {
    pub(crate) runs: BTreeMap<JobKey, Run>,
    pub(crate) orders: BTreeMap<JobKey, OrderRow>,
    pub(crate) machines: BTreeMap<String, MachineRow>,
    pub(crate) codes: BTreeMap<String, JoinCodeRow>,
    pub(crate) runners: BTreeMap<String, RunnerRow>,
    pub(crate) results: MemoryOutbox,
}

fn missing(key: &JobKey) -> StoreError {
    StoreError::NotFound(format!("job {}/{}", key.client, key.job))
}

fn step_ended(state: StepState) -> bool {
    !matches!(state, StepState::Queued | StepState::Running)
}

impl BrokerState {
    fn run(&mut self, key: &JobKey) -> Result<&mut Run, StoreError> {
        self.runs.get_mut(key).ok_or_else(|| missing(key))
    }

    pub(crate) fn accept(&mut self, intake: Intake) -> Result<Admission, StoreError> {
        let key = intake.job.key.clone();
        if self.runs.contains_key(&key) {
            return Ok(Admission::Duplicate);
        }
        if intake.order.key != key || intake.job.state != JobState::Queued {
            return Err(StoreError::Conflict(
                "intake needs a queued job and an order of the same key".into(),
            ));
        }
        self.orders.insert(key.clone(), intake.order.clone());
        self.runs.insert(
            key,
            Run {
                record: JobRecord {
                    job: intake.job,
                    steps: intake.steps,
                    spec: intake.spec,
                    github: intake.github,
                    log: None,
                },
                log: Vec::new(),
                truncated: false,
                order: intake.order,
                received: BTreeMap::new(),
                to_runner: 0,
            },
        );
        Ok(Admission::Accepted)
    }

    pub(crate) fn orders(&self) -> Vec<OrderRow> {
        let mut out: Vec<OrderRow> = self.orders.values().cloned().collect();
        out.sort_by(|a, b| (a.enqueued_at, &a.key).cmp(&(b.enqueued_at, &b.key)));
        out
    }

    pub(crate) fn grant(&mut self, key: &JobKey, runner: &str) -> Result<(), StoreError> {
        if !self.orders.contains_key(key) {
            return Err(StoreError::NotFound(format!(
                "order {}/{}",
                key.client, key.job
            )));
        }
        self.run(key)?.record.job.runner = Some(runner.to_string());
        self.orders.remove(key);
        Ok(())
    }

    pub(crate) fn requeue(&mut self, key: &JobKey) -> Result<(), StoreError> {
        let key = key.clone();
        if self.orders.contains_key(&key) {
            return Err(StoreError::Duplicate(format!(
                "order {}/{}",
                key.client, key.job
            )));
        }
        let run = self.run(&key)?;
        let order = run.order.clone();
        let job = &mut run.record.job;
        if job.state != JobState::Queued || job.started_at.is_some() {
            return Err(StoreError::Conflict(format!(
                "job {} already started, cannot be queued again",
                key.job
            )));
        }
        job.runner = None;
        self.orders.insert(key, order);
        Ok(())
    }

    pub(crate) fn transition(
        &mut self,
        key: &JobKey,
        change: Transition,
    ) -> Result<Applied, StoreError> {
        let job = &mut self.run(key)?.record.job;
        if job.state.is_final() {
            return Ok(Applied::Ignored);
        }
        if !job.state.can_become(change.state) {
            return Err(StoreError::InvalidTransition {
                from: job.state,
                to: change.state,
            });
        }
        if change.state == JobState::Running && job.runner.is_none() {
            return Err(StoreError::Conflict(format!(
                "job {} is not granted",
                key.job
            )));
        }
        if change.state == JobState::Running {
            job.started_at = Some(change.at);
        }
        if change.state.is_final() {
            job.finished_at = Some(change.at);
            job.exit_code = change.exit_code;
        }
        if job.cancelled_by.is_none() {
            job.cancelled_by = change.cancelled_by;
        }
        if job.cancel_reason.is_none() {
            job.cancel_reason = change.cancel_reason;
        }
        job.state = change.state;
        if change.state.is_final() {
            self.orders.remove(key);
        }
        Ok(Applied::Changed)
    }

    pub(crate) fn update_step(
        &mut self,
        key: &JobKey,
        update: StepUpdate,
    ) -> Result<Applied, StoreError> {
        let record = &mut self.run(key)?.record;
        if record.job.state.is_final() {
            return Ok(Applied::Ignored);
        }
        let step = record
            .steps
            .iter_mut()
            .find(|s| s.ord == update.ord)
            .ok_or_else(|| StoreError::NotFound(format!("step {} of {}", update.ord, key.job)))?;
        if step_ended(step.state) {
            return Ok(Applied::Ignored);
        }
        step.state = update.state;
        step.exit_code = update.exit_code.or(step.exit_code);
        step.started_at = update.started_at.or(step.started_at);
        step.finished_at = update.finished_at.or(step.finished_at);
        Ok(Applied::Changed)
    }

    pub(crate) fn accept_seq(
        &mut self,
        key: &JobKey,
        from: Inbound,
        seq: u64,
    ) -> Result<bool, StoreError> {
        accept_next(self.run(key)?.received.entry(from).or_insert(0), seq)
    }

    pub(crate) fn last_seq(&mut self, key: &JobKey, from: Inbound) -> Result<u64, StoreError> {
        Ok(self.run(key)?.received.get(&from).copied().unwrap_or(0))
    }

    pub(crate) fn next_seq_to_runner(&mut self, key: &JobKey) -> Result<u64, StoreError> {
        let run = self.run(key)?;
        run.to_runner += 1;
        Ok(run.to_runner)
    }

    pub(crate) fn append_log(
        &mut self,
        key: &JobKey,
        data: &[u8],
        at: Timestamp,
        limit: u64,
    ) -> Result<LogAppend, StoreError> {
        let run = self.run(key)?;
        if run.truncated {
            return Ok(LogAppend::Dropped);
        }
        let used = run.log.len() as u64;
        let result = if used + data.len() as u64 <= limit {
            run.log.extend_from_slice(data);
            LogAppend::Appended
        } else {
            let fits = limit.saturating_sub(used) as usize;
            run.log.extend_from_slice(&data[..fits]);
            run.log.extend_from_slice(LOG_TRUNCATED_MARK);
            run.truncated = true;
            LogAppend::Truncated
        };
        run.record.log = Some(JobLogRow {
            path: format!("logs/{}.log", key.job),
            bytes: run.log.len() as u64,
            updated_at: at,
        });
        Ok(result)
    }

    pub(crate) fn read_log(&mut self, key: &JobKey) -> Result<Vec<u8>, StoreError> {
        Ok(self.run(key)?.log.clone())
    }

    pub(crate) fn jobs(&self, filter: &JobFilter) -> Vec<JobRecord> {
        let mut out: Vec<JobRecord> = self
            .runs
            .values()
            .filter(|r| filter.matches(&r.record.job))
            .map(|r| r.record.clone())
            .collect();
        out.sort_by(|a, b| (b.job.queued_at, &b.job.key).cmp(&(a.job.queued_at, &a.job.key)));
        if let Some(limit) = filter.limit {
            out.truncate(limit);
        }
        out
    }

    pub(crate) fn prune(&mut self, limits: PruneLimits) -> PruneReport {
        let mut report = PruneReport::default();
        let age = |r: &Run| {
            r.record
                .job
                .finished_at
                .filter(|_| r.record.job.state.is_final())
                .map(|f| limits.now.millis_since(f))
        };
        let old_rows: Vec<JobKey> = self
            .runs
            .iter()
            .filter(|(_, r)| age(r).is_some_and(|a| a > limits.row_max_age_ms))
            .map(|(k, _)| k.clone())
            .collect();
        for key in old_rows {
            let run = self.runs.remove(&key).unwrap();
            if run.record.log.is_some() {
                report.logs_removed += 1;
                report.bytes_freed += run.log.len() as u64;
            }
            report.rows_removed += 1;
        }
        let old_logs: Vec<JobKey> = self
            .runs
            .iter()
            .filter(|(_, r)| {
                r.record.log.is_some() && age(r).is_some_and(|a| a > limits.log_max_age_ms)
            })
            .map(|(k, _)| k.clone())
            .collect();
        for key in old_logs {
            Self::drop_log(self.runs.get_mut(&key).unwrap(), &mut report);
        }
        let mut total: u64 = self.runs.values().map(|r| r.log.len() as u64).sum();
        let mut by_age: Vec<(Timestamp, JobKey)> = self
            .runs
            .iter()
            .filter(|(_, r)| r.record.log.is_some() && age(r).is_some())
            .map(|(k, r)| (r.record.job.finished_at.unwrap(), k.clone()))
            .collect();
        by_age.sort();
        for (_, key) in by_age {
            if total <= limits.log_max_bytes {
                break;
            }
            let run = self.runs.get_mut(&key).unwrap();
            total -= run.log.len() as u64;
            Self::drop_log(run, &mut report);
        }
        report
    }

    fn drop_log(run: &mut Run, report: &mut PruneReport) {
        report.logs_removed += 1;
        report.bytes_freed += run.log.len() as u64;
        run.log = Vec::new();
        run.record.log = None;
    }
}
