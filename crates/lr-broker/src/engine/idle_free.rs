//! Idle warm services as free memory in the reservation (spec 6, 3.11): with `evict_idle` the
//! planner also plans the queue as if idle services were already stopped. An order may start
//! now only if both plans start it, so no later order takes the room the head order gets once
//! the idle services stop; the head keeps waiting on memory, which makes the broker stop them.

use super::Engine;
use lr_core::{JobId, Plan, Timestamp};
use lr_proto::ServiceState;

impl Engine {
    /// Memory held by services that are starting or busy; a stopping service frees its memory.
    pub(crate) fn busy_service_mem_mb(&self) -> u64 {
        self.runners
            .values()
            .flat_map(|slot| slot.services.iter())
            .filter(|s| matches!(s.state, ServiceState::Starting | ServiceState::Busy))
            .fold(0u64, |a, s| a.saturating_add(s.mem_mb))
    }

    /// `held`, the plan with every live service holding its memory, narrowed to the orders the
    /// plan with idle services counted free also starts; an order dropped here waits with the
    /// reason that plan gives it.
    pub(crate) fn within_idle_free_plan(&self, now: Timestamp, mut held: Plan) -> Plan {
        let idle_free = self.plan_holding(now, self.busy_service_mem_mb());
        let starts = |job: &JobId| idle_free.assignments.iter().any(|a| &a.job == job);
        let (kept, dropped): (Vec<_>, Vec<_>) =
            held.assignments.into_iter().partition(|a| starts(&a.job));
        held.assignments = kept;
        for a in dropped {
            if let Some((_, wait)) = idle_free.waiting.iter().find(|(j, _)| *j == a.job) {
                held.waiting.push((a.job, wait.clone()));
            }
        }
        held
    }
}
