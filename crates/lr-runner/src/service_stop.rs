use crate::agent::Shared;
use crate::service_book::{Phase, POLL};
use lr_abstractions::{stop_signal, Level, Liveness, ProcessRef};
use std::sync::Arc;
use std::time::Duration;

enum Claim {
    Mine(ProcessRef),
    Busy,
    Unknown,
}

impl Shared {
    fn claim_stop(&self, name: &str) -> Claim {
        let mut st = self.state();
        match st.services.warm.get_mut(name) {
            Some(w) if w.phase == Phase::Idle => {
                w.phase = Phase::Stopping;
                Claim::Mine(w.process.clone())
            }
            Some(_) => Claim::Busy,
            None => Claim::Unknown,
        }
    }

    /// Stops a warm service (eviction, end of its lifetime, runner stop). A service that
    /// serves a job or is already stopping is left alone; a service known only from the
    /// registry is killed.
    pub(crate) async fn stop_service(&self, name: &str) {
        match self.claim_stop(name) {
            Claim::Mine(process) => self.finish_stop(name, process).await,
            Claim::Busy => self.diag.event(
                Level::Info,
                "service serves a job or is stopping, not stopped",
                &[("service", name)],
            ),
            Claim::Unknown => {
                self.services().stop(name).await;
                self.services_touched();
            }
        }
    }

    fn alive(&self, process: &ProcessRef) -> bool {
        process.start.is_some_and(|start| {
            self.deps.inspector.liveness(process.pid, start) == Liveness::Alive
        })
    }

    /// Runs `stop`, gives the service `stop_timeout_s` to end, then kills whatever is left of
    /// its tree; the caller has set the service to stopping.
    pub(crate) async fn finish_stop(&self, name: &str, process: ProcessRef) {
        self.record_service(name).await;
        if let Some(spec) = self.service_spec(name) {
            if let Some(command) = &spec.stop {
                let deadline = self.now().plus_secs(spec.stop_timeout_s);
                let (_never, signal) = stop_signal();
                self.service_command(name, "stop", command, spec.stop_timeout_s, signal)
                    .await;
                loop {
                    let left = Duration::from_millis(deadline.millis_since(self.now()));
                    if left.is_zero() || !self.alive(&process) {
                        break;
                    }
                    self.deps.clock.sleep(left.min(POLL)).await;
                }
                if self.alive(&process) {
                    self.diag.event(
                        Level::Warn,
                        "service did not stop within the grace, killed",
                        &[("service", name)],
                    );
                }
            }
        }
        self.kill_service_tree(name, &process).await;
        self.drop_service(name).await;
        self.diag
            .event(Level::Info, "service stopped", &[("service", name)]);
    }

    /// Stops every idle service and every service left in the registry (runner stop).
    pub(crate) async fn stop_all_services(&self) {
        let mut names: Vec<String> = self.state().services.warm.keys().cloned().collect();
        match self.deps.store.services().await {
            Ok(rows) => names.extend(rows.into_iter().map(|r| r.name)),
            Err(e) => self.diag.event(
                Level::Error,
                "service registry unreadable",
                &[("error", &e.to_string())],
            ),
        }
        names.sort();
        names.dedup();
        for name in names {
            self.stop_service(&name).await;
        }
    }

    /// Stops a service in its own task, so a session or a timer that ends meanwhile does not
    /// cut the stop short.
    pub(crate) fn stop_service_detached(self: &Arc<Self>, name: String) {
        let shared = self.clone();
        tokio::spawn(async move { shared.stop_service(&name).await });
    }
}

/// Ends idle services when their idle timeout or `max_lifetime_s` is reached.
pub(crate) async fn service_reaper<T>(shared: &Arc<Shared>) -> T {
    loop {
        let changed = shared.services_changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        match shared.next_expiry() {
            Some((at, name)) if shared.now() >= at => {
                if let Claim::Mine(process) = shared.claim_stop(&name) {
                    let s = shared.clone();
                    tokio::spawn(async move { s.finish_stop(&name, process).await });
                }
            }
            Some((at, _)) => tokio::select! {
                _ = shared.deps.clock.sleep_until(at) => {}
                _ = &mut changed => {}
            },
            None => changed.await,
        }
    }
}
