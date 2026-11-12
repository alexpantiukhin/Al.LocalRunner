use crate::agent::Shared;
use crate::job::Final;
use crate::prepare::{base_env, secret_var};
use crate::service_book::{expires, Phase, Warm, POLL};
use crate::service_config::{Lifetime, Reuse, ServiceSpec};
use lr_abstractions::metric::{SERVICE_FAILURES_TOTAL, SERVICE_STARTS_TOTAL};
use lr_abstractions::{Level, Liveness, ProcessRef, SpawnSpec, Stop, StopSignal};
use lr_core::{JobState, Timestamp};
use lr_proto::{AssignSpec, ReasonCode};
use std::time::Duration;

enum Claim {
    Reused,
    Start,
    Replace(ProcessRef),
    Crashed(ProcessRef),
    Wait,
    Unavailable,
}

enum Ready {
    Yes,
    Timeout,
    Stopped,
}

fn service_failed(detail: String) -> Final {
    Final {
        reason: Some(ReasonCode::ServiceFailed),
        ..Final::failed(detail)
    }
}

fn cancelled(name: &str) -> Final {
    Final {
        state: JobState::Cancelled,
        exit_code: None,
        reason: None,
        detail: Some(format!("cancelled while service {name} started")),
        peak_mem_mb: None,
        peak_disk_mb: None,
    }
}

fn reusable(reuse: Reuse, w: &Warm, client: &str, project: &str) -> bool {
    match reuse {
        Reuse::SameClient => w.client == client,
        Reuse::SameProject => w.project == project,
        Reuse::Any => true,
    }
}

impl Shared {
    /// Starts or reuses every service the job needs; the names held so far come back also on
    /// failure, so they are released.
    pub(crate) async fn acquire_services(
        &self,
        spec: &AssignSpec,
        stop: &StopSignal,
    ) -> (Vec<String>, Result<(), Final>) {
        let mut held: Vec<String> = Vec::new();
        for name in &spec.requirements.services {
            if held.contains(name) {
                continue;
            }
            if let Err(f) = self.acquire(name, &spec.client, &spec.project, stop).await {
                return (held, Err(f));
            }
            held.push(name.clone());
        }
        (held, Ok(()))
    }

    fn claim_service(&self, name: &str, spec: &ServiceSpec, client: &str, project: &str) -> Claim {
        let now = self.now();
        let mut st = self.state();
        if st.services.unavailable.contains(name) {
            return Claim::Unavailable;
        }
        let Some(w) = st.services.warm.get_mut(name) else {
            return Claim::Start;
        };
        if w.phase != Phase::Idle {
            return Claim::Wait;
        }
        let alive = match w.process.start {
            Some(start) => self.deps.inspector.liveness(w.process.pid, start),
            None => Liveness::Unknown,
        };
        if alive == Liveness::Dead {
            w.phase = Phase::Stopping;
            return Claim::Crashed(w.process.clone());
        }
        if now >= expires(spec, w.started_at) || !reusable(spec.reuse, w, client, project) {
            w.phase = Phase::Stopping;
            return Claim::Replace(w.process.clone());
        }
        w.phase = Phase::Busy;
        Claim::Reused
    }

    async fn acquire(
        &self,
        name: &str,
        client: &str,
        project: &str,
        stop: &StopSignal,
    ) -> Result<(), Final> {
        let Some(spec) = self.service_spec(name) else {
            return Err(service_failed(format!(
                "service {name} is not declared on this runner"
            )));
        };
        loop {
            let changed = self.services_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            match self.claim_service(name, spec, client, project) {
                Claim::Reused => {
                    self.record_service(name).await;
                    self.diag
                        .event(Level::Info, "warm service reused", &[("service", name)]);
                    return Ok(());
                }
                Claim::Start => break,
                Claim::Replace(process) => self.finish_stop(name, process).await,
                Claim::Crashed(process) => {
                    self.kill_service_tree(name, &process).await;
                    self.drop_service(name).await;
                    self.failure(name, spec, format!("service {name} crashed"))?;
                    break;
                }
                Claim::Wait => changed.await,
                Claim::Unavailable => {
                    return Err(service_failed(format!(
                        "service {name} is unavailable: its restart limit is reached"
                    )))
                }
            }
        }
        self.start(name, spec, client, project, stop).await
    }

    fn failure(&self, name: &str, spec: &ServiceSpec, detail: String) -> Result<(), Final> {
        self.service_metric(SERVICE_FAILURES_TOTAL, name);
        let exhausted = {
            let mut st = self.state();
            let n = st.services.failures.entry(name.to_string()).or_insert(0);
            *n += 1;
            let exhausted = *n > spec.restart;
            if exhausted {
                st.services.unavailable.insert(name.to_string());
                st.caps_dirty = true;
            }
            exhausted
        };
        self.diag.event(
            Level::Warn,
            "service failed",
            &[("service", name), ("why", &detail)],
        );
        if exhausted {
            self.diag.event(
                Level::Error,
                "service restart limit reached, service unavailable",
                &[("service", name)],
            );
            self.wake.notify_one();
            return Err(service_failed(detail));
        }
        Ok(())
    }

    async fn start(
        &self,
        name: &str,
        spec: &ServiceSpec,
        client: &str,
        project: &str,
        stop: &StopSignal,
    ) -> Result<(), Final> {
        let mut secrets = Vec::new();
        for secret in &spec.secrets {
            match self.deps.secrets.get(secret).await {
                Ok(v) => secrets.push((secret_var(secret), v)),
                Err(e) => {
                    return Err(Final {
                        reason: Some(ReasonCode::SecretMissing),
                        ..Final::failed(format!("service {name}: {e}"))
                    })
                }
            }
        }
        let cwd = self.service_dir(name);
        if let Err(e) = self.fresh_dir(&cwd).await {
            let detail = format!("service {name} directory not created: {e}");
            self.failure(name, spec, detail.clone())?;
            return Err(service_failed(detail));
        }
        self.service_metric(SERVICE_STARTS_TOTAL, name);
        self.diag
            .event(Level::Info, "service starting", &[("service", name)]);
        let launch = SpawnSpec {
            command: spec.start.clone(),
            cwd,
            env: base_env(self),
            secrets,
        };
        let process = match self.deps.process.spawn(launch).await {
            Ok(p) => p,
            Err(e) => {
                let detail = format!("service {name} did not start: {e}");
                self.failure(name, spec, detail.clone())?;
                return Err(service_failed(detail));
            }
        };
        let now = self.now();
        self.state().services.warm.insert(
            name.to_string(),
            Warm {
                process: process.clone(),
                started_at: now,
                client: client.to_string(),
                project: project.to_string(),
                phase: Phase::Starting,
                idle_since: now,
            },
        );
        self.record_service(name).await;
        match self.ready(name, spec, stop).await {
            Ready::Yes => {
                self.set_phase(name, Phase::Busy);
                self.record_service(name).await;
                self.service_up(name, true);
                self.diag
                    .event(Level::Info, "service ready", &[("service", name)]);
                Ok(())
            }
            Ready::Timeout => {
                self.kill_service_tree(name, &process).await;
                self.drop_service(name).await;
                let detail = format!(
                    "service {name} did not pass ready_check within {} s",
                    spec.ready_timeout_s
                );
                self.failure(name, spec, detail.clone())?;
                Err(service_failed(detail))
            }
            Ready::Stopped => {
                self.kill_service_tree(name, &process).await;
                self.drop_service(name).await;
                Err(cancelled(name))
            }
        }
    }

    async fn fresh_dir(&self, dir: &std::path::Path) -> Result<(), lr_abstractions::FilesError> {
        if self.deps.files.exists(dir).await? {
            self.deps.files.remove_dir_all(dir).await?;
        }
        self.deps.files.create_dir_all(dir).await
    }

    async fn ready(&self, name: &str, spec: &ServiceSpec, stop: &StopSignal) -> Ready {
        let deadline = self.now().plus_secs(spec.ready_timeout_s);
        loop {
            let left_ms = deadline.millis_since(self.now());
            if left_ms == 0 {
                return Ready::Timeout;
            }
            let out = self
                .service_command(
                    name,
                    "ready",
                    &spec.ready_check,
                    left_ms.div_ceil(1000),
                    stop.clone(),
                )
                .await;
            if stop.current().is_some() {
                return Ready::Stopped;
            }
            if out.state == JobState::Passed {
                return Ready::Yes;
            }
            let left = Duration::from_millis(deadline.millis_since(self.now()));
            if left.is_zero() {
                return Ready::Timeout;
            }
            let mut stopped = stop.clone();
            tokio::select! {
                _ = self.deps.clock.sleep(left.min(POLL)) => {}
                _ = stopped.reached(Stop::Cancel) => return Ready::Stopped,
            }
        }
    }

    /// Hands the services of an ended job back: `per_job` ones and ones past
    /// `max_lifetime_s` are stopped, the others stay warm and idle.
    pub(crate) async fn release_services(&self, held: &[String]) {
        let now = self.now();
        for name in held {
            let Some(spec) = self.service_spec(name) else {
                continue;
            };
            let process = {
                let mut st = self.state();
                let Some(w) = st.services.warm.get_mut(name) else {
                    continue;
                };
                if spec.lifetime == Lifetime::PerJob || now >= expires(spec, w.started_at) {
                    w.phase = Phase::Stopping;
                    Some(w.process.clone())
                } else {
                    None
                }
            };
            match process {
                Some(p) => self.finish_stop(name, p).await,
                None => {
                    self.set_phase(name, Phase::Idle);
                    self.record_service(name).await;
                }
            }
        }
    }

    /// When the next idle service must end and its name.
    pub(crate) fn next_expiry(&self) -> Option<(Timestamp, String)> {
        let st = self.state();
        st.services
            .warm
            .iter()
            .filter(|(_, w)| w.phase == Phase::Idle)
            .filter_map(|(name, w)| {
                let spec = self.service_spec(name)?;
                let mut at = expires(spec, w.started_at);
                if spec.lifetime == Lifetime::Idle {
                    if let Some(idle) = spec.idle_timeout_s {
                        at = at.min(w.idle_since.plus_secs(idle));
                    }
                }
                Some((at, name.clone()))
            })
            .min()
    }
}
