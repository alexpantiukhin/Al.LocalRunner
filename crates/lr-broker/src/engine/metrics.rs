//! Metrics of the broker (spec 12.2): counters and histograms at the events, gauges recomputed
//! from the engine state after every plan and every tick.

use super::{Engine, Job};
use lr_abstractions::metric::*;
use lr_abstractions::Level;
use lr_core::{JobId, JobState, Timestamp, Wait};
use std::collections::{BTreeMap, BTreeSet};

/// Saturation reasons of `saturation_wait_seconds`, always all reported.
pub(crate) const SATURATION_REASONS: [&str; 5] = ["runner_busy", "memory", "cpu", "tag", "rule"];

#[derive(Default)]
pub(crate) struct Gauges {
    platforms: BTreeSet<String>,
    saturation: BTreeMap<&'static str, Timestamp>,
    next_outbox: Timestamp,
}

pub(crate) fn result_of(state: JobState) -> Option<&'static str> {
    match state {
        JobState::Passed => Some("passed"),
        JobState::Failed => Some("failed"),
        JobState::Timeout => Some("timeout"),
        JobState::Cancelled => Some("cancelled"),
        JobState::Lost => Some("lost"),
        JobState::Queued | JobState::Running | JobState::Cancelling => None,
    }
}

pub(crate) fn saturation_reason(wait: &Wait) -> Option<&'static str> {
    match wait {
        Wait::RunnerBusy => Some("runner_busy"),
        Wait::Memory { .. } => Some("memory"),
        Wait::Cpu { .. } => Some("cpu"),
        Wait::Exclusive { .. } => Some("tag"),
        Wait::Reserved { .. } | Wait::Quota { .. } => Some("rule"),
        Wait::RunnerOffline | Wait::NoRunner | Wait::HeadReserved { .. } => None,
    }
}

fn label_of(job: &Job, known: &BTreeSet<&str>) -> String {
    match &job.entry.order.requirements.platform {
        None => SAME_MACHINE.to_string(),
        Some(p) if known.contains(p.as_str()) => p.clone(),
        Some(_) => OTHER_PLATFORM.to_string(),
    }
}

fn seconds(ms: u64) -> f64 {
    ms as f64 / 1000.0
}

impl Engine {
    fn known_platforms(&self) -> BTreeSet<&str> {
        self.runners
            .values()
            .flat_map(|r| r.caps.platforms.iter().map(String::as_str))
            .collect()
    }

    fn platform_label(&self, job: &Job) -> String {
        label_of(job, &self.known_platforms())
    }

    fn with_client<'a>(
        job: &'a Job,
        mut labels: Vec<(&'a str, &'a str)>,
    ) -> Vec<(&'a str, &'a str)> {
        let client = job.entry.client.label.as_str();
        if !client.is_empty() {
            labels.push((CLIENT, client));
        }
        labels
    }

    /// A refused order: `orders_total{result="rejected"}`.
    pub(crate) fn count_rejected(&self, job: &Job) {
        let platform = self.platform_label(job);
        let labels = Self::with_client(job, vec![(RESULT, "rejected"), (PLATFORM, &platform)]);
        self.meter.add(ORDERS_TOTAL, &labels, 1);
    }

    /// A run reached its final `state`: `orders_total` and, when it started,
    /// `job_duration_seconds`.
    pub(crate) fn count_end(&self, job: &Job, state: JobState) {
        let Some(result) = result_of(state) else {
            return;
        };
        let platform = self.platform_label(job);
        let labels = Self::with_client(job, vec![(RESULT, result), (PLATFORM, &platform)]);
        self.meter.add(ORDERS_TOTAL, &labels, 1);
        if let Some(started) = job.started_at {
            let took = seconds(self.now().millis_since(started));
            self.meter.observe(JOB_DURATION_SECONDS, &labels, took);
        }
    }

    /// An order leaves the queue for a runner: `queue_wait_seconds`.
    pub(crate) fn observe_wait(&self, job: &Job, now: Timestamp) {
        let platform = self.platform_label(job);
        let labels = Self::with_client(job, vec![(PLATFORM, &platform)]);
        let waited = seconds(now.millis_since(job.spec.enqueued_at));
        self.meter.observe(QUEUE_WAIT_SECONDS, &labels, waited);
    }

    /// Remembers since when the longest-waiting order of each saturation reason waits.
    pub(crate) fn note_waits(&mut self, waiting: &[(JobId, Wait)]) {
        let mut since: BTreeMap<&'static str, Timestamp> = BTreeMap::new();
        for (id, wait) in waiting {
            let Some(reason) = saturation_reason(wait) else {
                continue;
            };
            let Some(job) = self.queue.iter().find(|j| &j.internal == id) else {
                continue;
            };
            let at = job.spec.enqueued_at;
            since
                .entry(reason)
                .and_modify(|t| *t = (*t).min(at))
                .or_insert(at);
        }
        self.gauges.saturation = since;
    }

    /// Sets every gauge from the current state: queue, pool load per runner, runner
    /// presence, saturation.
    pub(crate) fn report(&mut self) {
        let now = self.now();
        let known = self.known_platforms();
        let mut queued: BTreeMap<String, u64> = BTreeMap::new();
        for job in &self.queue {
            *queued.entry(label_of(job, &known)).or_default() += 1;
        }
        drop(known);
        self.gauges.platforms.extend(queued.keys().cloned());
        for platform in &self.gauges.platforms {
            let n = queued.get(platform).copied().unwrap_or(0);
            self.meter
                .set(QUEUE_LENGTH, &[(PLATFORM, platform)], n as f64);
        }
        for (name, slot) in &self.runners {
            let (mut jobs, mut mem, mut cpu) = (0u64, 0u64, 0u64);
            for job in self
                .active
                .iter()
                .filter(|j| j.runner.as_deref() == Some(name))
            {
                let req = &job.entry.order.requirements;
                jobs += 1;
                mem = mem.saturating_add(req.mem_mb);
                cpu = cpu.saturating_add(u64::from(req.cpu));
            }
            let labels = [(POOL, POOL_VALUE), (RUNNER, name.as_str())];
            self.meter.set(RUNNING_JOBS, &labels, jobs as f64);
            self.meter.set(POOL_MEMORY_USED_MB, &labels, mem as f64);
            self.meter.set(POOL_CPU_USED, &labels, cpu as f64);
            let online = if slot.conn.is_some() { 1.0 } else { 0.0 };
            self.meter
                .set(RUNNER_ONLINE, &[(RUNNER, name.as_str())], online);
        }
        for reason in SATURATION_REASONS {
            let waited = self
                .gauges
                .saturation
                .get(reason)
                .map_or(0.0, |since| seconds(now.millis_since(*since)));
            self.meter
                .set(SATURATION_WAIT_SECONDS, &[(REASON, reason)], waited);
        }
    }

    /// Every [`OUTBOX_SAMPLE_S`] seconds: the size and age of the result outbox.
    pub(crate) async fn sample_outbox(&mut self) {
        let now = self.now();
        if now < self.gauges.next_outbox {
            return;
        }
        self.gauges.next_outbox = now.plus_secs(OUTBOX_SAMPLE_S);
        match self.store.results_backlog().await {
            Ok(b) => {
                let labels = [(ROLE, "broker")];
                self.meter.set(OUTBOX_PENDING, &labels, b.pending as f64);
                let age = b.oldest.map_or(0.0, |t| seconds(now.millis_since(t)));
                self.meter.set(OUTBOX_OLDEST_AGE_SECONDS, &labels, age);
            }
            Err(e) => self.diag(
                Level::Warn,
                "storage failed: could not measure the result outbox",
                &[("error", &e.to_string())],
            ),
        }
    }

    /// A runner left the registry: its series are dropped.
    pub(crate) fn forget_runner(&self, name: &str) {
        self.meter.forget(RUNNER, name);
    }

    /// `policy_reload_total`.
    pub(crate) fn count_reload(&self, ok: bool) {
        let result = if ok { "ok" } else { "error" };
        self.meter.add(POLICY_RELOAD_TOTAL, &[(RESULT, result)], 1);
    }
}
