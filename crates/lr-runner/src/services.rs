use crate::diag::Diag;
use lr_abstractions::{
    Level, Liveness, ProcessInspector, ProcessRef, ProcessRunner, RunnerStore, ServiceRow,
    ServiceState,
};
use lr_core::Timestamp;
use lr_proto::{ServiceReport, ServicesReport};

pub(crate) struct Services<'a> {
    pub(crate) store: &'a dyn RunnerStore,
    pub(crate) process: &'a dyn ProcessRunner,
    pub(crate) inspector: &'a dyn ProcessInspector,
    pub(crate) diag: &'a Diag,
}

fn wire_state(s: ServiceState) -> lr_proto::ServiceState {
    match s {
        ServiceState::Starting => lr_proto::ServiceState::Starting,
        ServiceState::Idle => lr_proto::ServiceState::Idle,
        ServiceState::Busy => lr_proto::ServiceState::Busy,
        ServiceState::Stopping => lr_proto::ServiceState::Stopping,
        ServiceState::Stopped => lr_proto::ServiceState::Stopped,
        ServiceState::Failed => lr_proto::ServiceState::Failed,
        ServiceState::Unavailable => lr_proto::ServiceState::Unavailable,
    }
}

impl Services<'_> {
    async fn kill(&self, row: &ServiceRow) -> bool {
        let pid = row.pid.to_string();
        let fields = [("service", row.name.as_str()), ("pid", pid.as_str())];
        let Some(start) = row.pid_start else {
            self.diag.event(
                Level::Warn,
                "service process has no start time, not killed",
                &fields,
            );
            return false;
        };
        match self.inspector.liveness(row.pid, start) {
            Liveness::Alive => {
                let process = ProcessRef {
                    pid: row.pid,
                    start: Some(start),
                };
                match self.process.kill_tree(&process).await {
                    Ok(()) => {
                        self.diag.event(Level::Info, "service killed", &fields);
                        true
                    }
                    Err(e) => {
                        self.diag.event(
                            Level::Error,
                            "service not killed",
                            &[("service", row.name.as_str()), ("error", &e.to_string())],
                        );
                        false
                    }
                }
            }
            Liveness::Dead => true,
            Liveness::Unknown => {
                self.diag.event(
                    Level::Warn,
                    "service process liveness unknown, not killed",
                    &fields,
                );
                false
            }
        }
    }

    pub(crate) async fn kill_orphans(&self) {
        let rows = match self.store.services().await {
            Ok(r) => r,
            Err(e) => {
                return self.diag.event(
                    Level::Error,
                    "service registry unreadable",
                    &[("error", &e.to_string())],
                )
            }
        };
        for row in rows {
            if self.kill(&row).await {
                self.forget(&row.name).await;
            }
        }
    }

    async fn forget(&self, name: &str) {
        if let Err(e) = self.store.remove_service(name).await {
            self.diag.event(
                Level::Error,
                "service not removed from the registry",
                &[("service", name), ("error", &e.to_string())],
            );
        }
    }

    pub(crate) async fn stop(&self, name: &str) {
        let rows = match self.store.services().await {
            Ok(r) => r,
            Err(e) => {
                return self.diag.event(
                    Level::Error,
                    "service registry unreadable, service not stopped",
                    &[("service", name), ("error", &e.to_string())],
                )
            }
        };
        match rows.iter().find(|r| r.name == name) {
            Some(row) => {
                if self.kill(row).await {
                    self.forget(name).await;
                }
            }
            None => self.diag.event(
                Level::Info,
                "stop of an unknown service",
                &[("service", name)],
            ),
        }
    }

    pub(crate) async fn report(&self, now: Timestamp) -> ServicesReport {
        let rows = match self.store.services().await {
            Ok(r) => r,
            Err(e) => {
                self.diag.event(
                    Level::Error,
                    "service registry unreadable",
                    &[("error", &e.to_string())],
                );
                Vec::new()
            }
        };
        ServicesReport {
            services: rows
                .into_iter()
                .map(|r| ServiceReport {
                    name: r.name,
                    state: wire_state(r.state),
                    mem_mb: r.mem_mb,
                    remaining_s: Some(r.expires_at.millis_since(now) / 1000),
                })
                .collect(),
        }
    }
}
