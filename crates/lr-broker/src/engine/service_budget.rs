//! Memory of runner services in the pool budget (spec 3.11): a live service holds its
//! `memory_mb`, and an order that will start a service reserves that memory before it is
//! assigned.

use super::runners::RunnerSlot;
use super::Engine;
use lr_core::{
    ClientView, Order, Policy, PoolLimits, QueueEntry, Reject, Requirements, RunningJob, ServeLog,
    Timestamp, Wait, Zone,
};
use lr_proto::ServiceState;

/// The policy with the memory of live runner services taken out of its pool, so the planner
/// never gives that memory to an order while the service lives. Reserve rules see it too.
pub(crate) struct HeldPool<'a> {
    pub(crate) inner: &'a dyn Policy,
    pub(crate) pool: PoolLimits,
    pub(crate) held_mb: u64,
}

impl Policy for HeldPool<'_> {
    fn order(&self, queue: &[QueueEntry], served: &ServeLog, now: Timestamp) -> Vec<usize> {
        self.inner.order(queue, served, now)
    }

    fn admit(&self, order: &Order, client: &ClientView) -> Result<(), Reject> {
        self.inner.admit(order, client)
    }

    fn gate(&self, entry: &QueueEntry, running: &[&QueueEntry]) -> Result<(), Wait> {
        self.inner.gate(entry, running)?;
        if self.held_mb == 0 {
            return Ok(());
        }
        let mut held = entry.clone();
        let mem = &mut held.order.requirements.mem_mb;
        *mem = mem.saturating_add(self.held_mb);
        match self.inner.gate(&held, running) {
            Err(w @ Wait::Reserved { .. }) => Err(w),
            _ => Ok(()),
        }
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

fn has_live(slot: &RunnerSlot, service: &str) -> bool {
    slot.services
        .iter()
        .any(|s| s.name == service && live(s.state))
}

fn offers_all(slot: &RunnerSlot, req: &Requirements) -> bool {
    req.services
        .iter()
        .all(|s| slot.wire.services.contains_key(s))
}

/// Memory the services of `req` would add on `slot`: the declared `memory_mb` of each
/// service the runner does not already report live (a live one is held already, and a warm
/// one that may not be reused is stopped before a fresh one starts).
fn to_start_mb(slot: &RunnerSlot, req: &Requirements) -> u64 {
    let mut seen: Vec<&String> = Vec::new();
    let mut mb = 0u64;
    for service in &req.services {
        if seen.contains(&service) || has_live(slot, service) {
            continue;
        }
        seen.push(service);
        let declared = slot.wire.service_mem_mb.get(service).copied().unwrap_or(0);
        mb = mb.saturating_add(declared);
    }
    mb
}

fn with_extra(mut entry: QueueEntry, mb: u64) -> QueueEntry {
    let mem = &mut entry.order.requirements.mem_mb;
    *mem = mem.saturating_add(mb);
    entry
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

    /// Memory of the live services of runner `name`.
    pub(crate) fn runner_service_mem_mb(&self, name: &str) -> u64 {
        self.runners.get(name).map_or(0, |slot| {
            slot.services
                .iter()
                .filter(|s| live(s.state))
                .fold(0u64, |a, s| a.saturating_add(s.mem_mb))
        })
    }

    /// Memory the services of a running job on `runner` will hold once they start: counted
    /// until the runner reports them, then held through [`Engine::service_mem_mb`].
    pub(crate) fn pending_service_mb(&self, runner: &str, req: &Requirements) -> u64 {
        self.runners
            .get(runner)
            .map_or(0, |slot| to_start_mb(slot, req))
    }

    /// The queue as the planner sees it: an order that needs services asks for their memory
    /// as well, the most any connected runner offering them would start.
    pub(crate) fn planned_queue(&self) -> Vec<QueueEntry> {
        self.queue
            .iter()
            .map(|j| {
                let req = &j.entry.order.requirements;
                if req.services.is_empty() {
                    return j.entry.clone();
                }
                let extra = self
                    .runners
                    .values()
                    .filter(|s| s.conn.is_some() && offers_all(s, req))
                    .map(|s| to_start_mb(s, req))
                    .max()
                    .unwrap_or(0);
                with_extra(j.entry.clone(), extra)
            })
            .collect()
    }

    /// Running jobs as the planner sees them, with the memory of services not reported yet.
    pub(crate) fn planned_running(&self) -> Vec<RunningJob> {
        self.running_jobs()
            .into_iter()
            .map(|mut r| {
                let extra = self.pending_service_mb(&r.runner, &r.entry.order.requirements);
                r.entry = with_extra(r.entry, extra);
                r
            })
            .collect()
    }
}
