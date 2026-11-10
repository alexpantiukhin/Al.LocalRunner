use super::runners::row_of;
use super::Engine;
use lr_abstractions::{Level, RunnerState};
use lr_core::{Wait, Zone};
use lr_proto::{
    BrokerToRunner, QueueEvent, ServiceState, ServicesReport, StopService, WireCapabilities,
};

impl Engine {
    pub(crate) async fn capabilities_update(&mut self, name: &str, mut wire: WireCapabilities) {
        let now = self.now();
        let Some(slot) = self.runners.get_mut(name) else {
            return;
        };
        wire.machine = slot.wire.machine.clone();
        slot.caps = wire.clone().into_capabilities(Zone::Remote);
        slot.wire = wire;
        let (persisted, owner) = (slot.persisted, slot.owner.clone());
        if persisted {
            let slot = &self.runners[name];
            let mut row = row_of(name, &owner, &slot.wire, now);
            if slot.draining {
                row.state = RunnerState::Draining;
            }
            if let Err(e) = self.store.upsert_runner(row).await {
                self.diag(
                    Level::Error,
                    "storage failed: could not record the runner capabilities",
                    &[("runner", name), ("error", &e.to_string())],
                );
            }
        }
        self.plan().await;
    }

    pub(crate) async fn services_report(&mut self, name: &str, report: ServicesReport) {
        let Some(slot) = self.runners.get_mut(name) else {
            return;
        };
        let changed: Vec<_> = report
            .services
            .iter()
            .filter(|s| {
                !slot
                    .services
                    .iter()
                    .any(|o| o.name == s.name && o.state == s.state && o.mem_mb == s.mem_mb)
            })
            .cloned()
            .collect();
        slot.stopping.retain(|n| {
            report
                .services
                .iter()
                .any(|s| &s.name == n && s.state == ServiceState::Idle)
        });
        slot.services = report.services;
        for service in changed {
            self.publish(
                None,
                QueueEvent::ServiceChanged {
                    runner: name.to_string(),
                    service,
                },
            );
        }
        self.plan().await;
    }

    pub(crate) fn evict_idle(&mut self, head: Option<&Wait>) {
        if !self.policy.file().services.evict_idle
            || !matches!(head, Some(Wait::Memory { .. } | Wait::Cpu { .. }))
        {
            return;
        }
        let mut orders = Vec::new();
        for slot in self.runners.values_mut() {
            let Some(conn) = slot.conn else {
                continue;
            };
            for s in &slot.services {
                if s.state == ServiceState::Idle && slot.stopping.insert(s.name.clone()) {
                    orders.push((conn, s.name.clone()));
                }
            }
        }
        for (conn, name) in orders {
            self.send(conn, &BrokerToRunner::StopService(StopService { name }));
        }
    }
}
