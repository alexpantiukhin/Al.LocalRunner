use crate::agent::Shared;
use crate::prepare::base_env;
use crate::service_config::ServiceSpec;
use async_trait::async_trait;
use lr_abstractions::metric::{RUNNER, SERVICE, SERVICE_UP};
use lr_abstractions::{
    JobEvent, JobOutput, JobSpec, Level, ProcessRef, ServiceRow, ServiceState, DEFAULT_CANCEL_GRACE,
};
use lr_core::{JobId, StepSpec, Timestamp};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

pub(crate) const POLL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Starting,
    Busy,
    Idle,
    Stopping,
}

impl Phase {
    fn state(self) -> ServiceState {
        match self {
            Phase::Starting => ServiceState::Starting,
            Phase::Busy => ServiceState::Busy,
            Phase::Idle => ServiceState::Idle,
            Phase::Stopping => ServiceState::Stopping,
        }
    }
}

pub(crate) struct Warm {
    pub(crate) process: ProcessRef,
    pub(crate) started_at: Timestamp,
    pub(crate) client: String,
    pub(crate) project: String,
    pub(crate) phase: Phase,
    pub(crate) idle_since: Timestamp,
}

#[derive(Default)]
pub(crate) struct Book {
    pub(crate) warm: BTreeMap<String, Warm>,
    pub(crate) failures: BTreeMap<String, u32>,
    pub(crate) unavailable: BTreeSet<String>,
    pub(crate) dirty: bool,
}

pub(crate) fn expires(spec: &ServiceSpec, started_at: Timestamp) -> Timestamp {
    started_at.plus_secs(spec.max_lifetime_s)
}

struct Discard;

#[async_trait]
impl JobOutput for Discard {
    async fn emit(&self, _event: JobEvent) {}
}

impl Shared {
    pub(crate) fn service_spec(&self, name: &str) -> Option<&ServiceSpec> {
        self.deps.settings.services.get(name)
    }

    pub(crate) fn service_dir(&self, name: &str) -> PathBuf {
        self.deps.settings.state_dir.join("services").join(name)
    }

    pub(crate) fn service_labels<'a>(&'a self, name: &'a str) -> [(&'a str, &'a str); 2] {
        [(RUNNER, self.deps.settings.name.as_str()), (SERVICE, name)]
    }

    pub(crate) fn service_metric(&self, metric: &str, name: &str) {
        self.deps.meter.add(metric, &self.service_labels(name), 1);
    }

    pub(crate) fn service_up(&self, name: &str, up: bool) {
        let value = if up { 1.0 } else { 0.0 };
        self.deps
            .meter
            .set(SERVICE_UP, &self.service_labels(name), value);
    }

    pub(crate) fn services_touched(&self) {
        self.state().services.dirty = true;
        self.wake.notify_one();
        self.services_changed.notify_waiters();
    }

    pub(crate) fn set_phase(&self, name: &str, phase: Phase) {
        if let Some(w) = self.state().services.warm.get_mut(name) {
            w.phase = phase;
            if phase == Phase::Idle {
                w.idle_since = self.deps.clock.now();
            }
        }
    }

    pub(crate) async fn record_service(&self, name: &str) {
        let row = {
            let st = self.state();
            let (Some(w), Some(spec)) = (st.services.warm.get(name), self.service_spec(name))
            else {
                return;
            };
            ServiceRow {
                name: name.to_string(),
                pid: w.process.pid,
                pid_start: w.process.start,
                state: w.phase.state(),
                started_at: w.started_at,
                expires_at: expires(spec, w.started_at),
                mem_mb: spec.memory_mb,
            }
        };
        if let Err(e) = self.deps.store.upsert_service(row).await {
            self.diag.event(
                Level::Error,
                "service not recorded in the registry",
                &[("service", name), ("error", &e.to_string())],
            );
        }
        self.services_touched();
    }

    pub(crate) async fn drop_service(&self, name: &str) {
        self.state().services.warm.remove(name);
        if let Err(e) = self.deps.store.remove_service(name).await {
            self.diag.event(
                Level::Error,
                "service not removed from the registry",
                &[("service", name), ("error", &e.to_string())],
            );
        }
        self.service_up(name, false);
        self.services_touched();
    }

    pub(crate) async fn forget_undeclared_services(&self) {
        let Ok(rows) = self.deps.store.services().await else {
            return;
        };
        for row in rows {
            if self.service_spec(&row.name).is_none() {
                self.deps.meter.forget(SERVICE, &row.name);
            }
        }
    }

    pub(crate) async fn kill_service_tree(&self, name: &str, process: &ProcessRef) {
        if let Err(e) = self.deps.process.kill_tree(process).await {
            self.diag.event(
                Level::Error,
                "service tree not killed",
                &[("service", name), ("error", &e.to_string())],
            );
        }
    }

    pub(crate) async fn service_command(
        &self,
        name: &str,
        purpose: &str,
        command: &str,
        timeout_s: u64,
        stop: lr_abstractions::StopSignal,
    ) -> lr_abstractions::JobOutcome {
        let step = StepSpec {
            name: purpose.to_string(),
            command: command.to_string(),
            timeout_s,
            mem_mb: None,
            cpu: None,
            tags: None,
            secrets: Vec::new(),
            continue_on_failure: false,
        };
        let mut spec = JobSpec::new(
            JobId(format!("service:{name}:{purpose}")),
            timeout_s,
            vec![step],
        );
        spec.cwd = self.service_dir(name);
        spec.env = base_env(self);
        spec.cancel_grace = DEFAULT_CANCEL_GRACE;
        self.deps.process.run_job(spec, &Discard, stop).await
    }
}
