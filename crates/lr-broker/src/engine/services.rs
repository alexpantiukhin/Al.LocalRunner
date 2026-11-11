use super::runners::row_of;
use super::Engine;
use lr_abstractions::{Level, RunnerState};
use lr_core::{
    ClientView, Order, Policy, PoolLimits, QueueEntry, Reject, ServeLog, Timestamp, Wait, Zone,
};
use lr_proto::{
    BrokerToRunner, QueueEvent, ServiceState, ServicesReport, StopService, WireCapabilities,
};

/// The policy with the memory of live runner services taken out of its pool, so the planner
/// never gives that memory to an order while the service lives (spec 3.11).
pub(crate) struct HeldPool<'a> {
    pub(crate) inner: &'a dyn Policy,
    pub(crate) pool: PoolLimits,
}

impl Policy for HeldPool<'_> {
    fn order(&self, queue: &[QueueEntry], served: &ServeLog, now: Timestamp) -> Vec<usize> {
        self.inner.order(queue, served, now)
    }

    fn admit(&self, order: &Order, client: &ClientView) -> Result<(), Reject> {
        self.inner.admit(order, client)
    }

    fn gate(&self, entry: &QueueEntry, running: &[&QueueEntry]) -> Result<(), Wait> {
        self.inner.gate(entry, running)
    }

    fn pool(&self) -> &PoolLimits {
        &self.pool
    }

    fn runner_zone_order(&self) -> &[Zone] {
        self.inner.runner_zone_order()
    }

    fn group_of(&self, client: &ClientView) -> String {
        self.inner.group_of(client)
    }
}

fn live(state: ServiceState) -> bool {
    matches!(
        state,
        ServiceState::Starting | ServiceState::Idle | ServiceState::Busy | ServiceState::Stopping
    )
}

impl Engine {
    /// Memory held by the live services the runners report, busy or idle.
    pub(crate) fn service_mem_mb(&self) -> u64 {
        self.runners
            .values()
            .flat_map(|slot| slot.services.iter())
            .filter(|s| live(s.state))
            .fold(0u64, |a, s| a.saturating_add(s.mem_mb))
    }

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
