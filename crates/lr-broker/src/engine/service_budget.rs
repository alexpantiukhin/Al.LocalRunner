//! Memory of runner services in the pool budget (spec 3.11): a live service holds its
//! `memory_mb`, and an order that will start a service reserves that memory before it is
//! assigned.

use super::runners::RunnerSlot;
use super::Engine;
use lr_core::{
    pick_next, ClientView, JobId, Order, Plan, Policy, PoolLimits, QueueEntry, QueueState, Reject,
    Requirements, RunningJob, ServeLog, Timestamp, Wait, Zone,
};
use lr_proto::ServiceState;
use std::collections::BTreeSet;

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

    fn admission(&self) -> lr_core::Admission {
        self.inner.admission()
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
/// requested service, once per name. With `warm` a service the runner reports live adds
/// nothing: it is held already (a live one that may not be reused is stopped before a fresh
/// one starts).
pub(crate) fn service_cost_mb(slot: &RunnerSlot, req: &Requirements, warm: bool) -> u64 {
    let mut seen: Vec<&String> = Vec::new();
    let mut mb = 0u64;
    for service in &req.services {
        if seen.contains(&service) || (warm && has_live(slot, service)) {
            continue;
        }
        seen.push(service);
        let declared = slot.wire.service_mem_mb.get(service).copied().unwrap_or(0);
        mb = mb.saturating_add(declared);
    }
    mb
}

/// The memory an order adds for its services (spec 3.11): the cheapest of `slots` that offer
/// every requested service, 0 when none does. Intake passes every registered runner and cold
/// costs; the planner passes the runners that take work and counts warm instances as reused.
pub(crate) fn cheapest_service_mb<'a>(
    slots: impl IntoIterator<Item = &'a RunnerSlot>,
    req: &Requirements,
    warm: bool,
) -> u64 {
    if req.services.is_empty() {
        return 0;
    }
    slots
        .into_iter()
        .filter(|s| offers_all(s, req))
        .map(|s| service_cost_mb(s, req, warm))
        .min()
        .unwrap_or(0)
}

pub(crate) fn with_extra(mut entry: QueueEntry, mb: u64) -> QueueEntry {
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
            .map_or(0, |slot| service_cost_mb(slot, req, true))
    }

    /// The queue as the planner sees it: an order that needs services asks for their memory
    /// as well, on the cheapest runner that takes work (see [`cheapest_service_mb`]).
    pub(crate) fn planned_queue(&self) -> Vec<QueueEntry> {
        let taking: Vec<&RunnerSlot> = self.runners.values().filter(|s| s.takes_work()).collect();
        self.queue
            .iter()
            .map(|j| {
                let req = &j.entry.order.requirements;
                let extra = cheapest_service_mb(taking.iter().copied(), req, true);
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

    /// One planning pass (spec 6) with the memory of services (spec 3.11). An order is
    /// planned with its cheapest service memory; when the pass puts it on a runner where its
    /// services cost more than the pool or that runner has left, that runner is left out of
    /// the pass and the pass is planned again, so a warm instance elsewhere is reused instead
    /// of over-committing the pool.
    pub(crate) fn plan_pass(&self, now: Timestamp) -> Plan {
        let held = self.plan_holding(now, self.service_mem_mb());
        let head_on_memory = matches!(held.waiting.first(), Some((_, Wait::Memory { .. })));
        if !self.policy.file().services.evict_idle || held.assignments.is_empty() || !head_on_memory
        {
            return held;
        }
        self.within_idle_free_plan(now, held)
    }

    pub(crate) fn plan_holding(&self, now: Timestamp, held_mb: u64) -> Plan {
        let entries = self.planned_queue();
        let running = self.planned_running();
        let inner = self.policy.policy();
        let mut pool = inner.pool().clone();
        pool.mem_mb = pool.mem_mb.saturating_sub(held_mb);
        let used = running.iter().fold(0u64, |a, r| {
            a.saturating_add(r.entry.order.requirements.mem_mb)
        });
        let free_mb = pool.mem_mb.saturating_sub(used);
        let policy = HeldPool {
            inner,
            pool,
            held_mb,
        };
        let mut excluded: BTreeSet<String> = BTreeSet::new();
        loop {
            let mut runners = self.runner_views();
            for view in &mut runners {
                if excluded.contains(&view.name) {
                    view.online = false;
                }
            }
            let state = QueueState {
                queue: &entries,
                runners: &runners,
                running: &running,
                served: &self.served,
            };
            let plan = pick_next(&policy, &self.site, &state, now);
            match self.costlier_runner(&plan, &entries, free_mb) {
                Some(name) if !excluded.contains(&name) => {
                    excluded.insert(name);
                }
                _ => return plan,
            }
        }
    }

    fn costlier_runner(&self, plan: &Plan, entries: &[QueueEntry], free_mb: u64) -> Option<String> {
        let planned = |job: &JobId| entries.iter().find(|e| &e.order.job == job);
        let mut free = plan
            .assignments
            .iter()
            .filter_map(|a| planned(&a.job))
            .fold(free_mb, |f, e| {
                f.saturating_sub(e.order.requirements.mem_mb)
            });
        for a in &plan.assignments {
            let Some(entry) = planned(&a.job) else {
                continue;
            };
            let Some(job) = self.queue.iter().find(|j| j.internal == a.job) else {
                continue;
            };
            let req = &job.entry.order.requirements;
            let planned_extra = entry.order.requirements.mem_mb.saturating_sub(req.mem_mb);
            let actual = self.pending_service_mb(&a.runner, req);
            if actual <= planned_extra {
                continue;
            }
            let more = actual - planned_extra;
            let fits_runner = self
                .runners
                .get(&a.runner)
                .is_some_and(|s| req.mem_mb.saturating_add(actual) <= s.caps.mem_mb);
            if more > free || !fits_runner {
                return Some(a.runner.clone());
            }
            free -= more;
        }
        None
    }
}
