use crate::admission::{same_host, ForeignLoadMode};
use crate::model::{JobId, Timestamp, Zone};
use crate::policy::Policy;
use crate::queue::{Assignment, QueueEntry, Reservation, RunnerView, RunningJob, ServeLog};
use crate::routing::{sort_by_preference, suitable};
use crate::scheduler::{Scheduler, Wait};
use crate::zone::Site;

/// What the broker knows when it plans: the queue, the runners, what runs, group usage.
#[derive(Debug, Clone, Copy)]
pub struct QueueState<'a> {
    /// Queued orders.
    pub queue: &'a [QueueEntry],
    /// Registered runners, online or not.
    pub runners: &'a [RunnerView],
    /// Jobs running now.
    pub running: &'a [RunningJob],
    /// Usage per group.
    pub served: &'a ServeLog,
}

/// Result of one planning pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Orders to start now, in dispatch order.
    pub assignments: Vec<Assignment>,
    /// Orders that stay queued, with the reason, in dispatch order.
    pub waiting: Vec<(JobId, Wait)>,
    /// The reserved earliest start of the head order, if it is blocked and predictable.
    pub reservation: Option<Reservation>,
}

#[derive(Debug, Clone, Copy)]
struct Slot<'a> {
    entry: &'a QueueEntry,
    runner: &'a str,
    end: Timestamp,
}

struct Planner<'a> {
    policy: &'a dyn Policy,
    site: &'a Site,
    runners: &'a [RunnerView],
    now: Timestamp,
}

impl<'a> Planner<'a> {
    fn start_now(
        &self,
        entry: &QueueEntry,
        slots: &[Slot<'_>],
    ) -> Result<Vec<(&'a RunnerView, Zone)>, Wait> {
        let mut any = false;
        let mut any_online = false;
        let mut free = Vec::new();
        for runner in self.runners {
            let Ok(zone) = suitable(entry, runner, self.site) else {
                continue;
            };
            any = true;
            if !runner.online {
                continue;
            }
            any_online = true;
            if !slots.iter().any(|s| s.runner == runner.name) {
                free.push((runner, zone));
            }
        }
        if !any {
            return Err(Wait::NoRunner);
        }
        if !any_online {
            return Err(Wait::RunnerOffline);
        }
        if free.is_empty() {
            return Err(Wait::RunnerBusy);
        }
        let mut sched = Scheduler::new(self.policy.pool().clone());
        for s in slots {
            sched.insert_running(&s.entry.order.job, &s.entry.order.requirements);
        }
        sched.can_grant(&entry.order.job, &entry.order.requirements)?;
        let running: Vec<&QueueEntry> = slots.iter().map(|s| s.entry).collect();
        self.policy.gate(entry, &running)?;
        sort_by_preference(&mut free, self.policy.runner_zone_order());
        Ok(free)
    }

    fn admit_load(
        &self,
        entry: &QueueEntry,
        candidates: Vec<(&'a RunnerView, Zone)>,
        slots: &[Slot<'_>],
        running: usize,
    ) -> Result<Vec<(&'a RunnerView, Zone)>, Wait> {
        let admission = self.policy.admission();
        if admission.foreign_load == ForeignLoadMode::Off {
            return Ok(candidates);
        }
        let req = &entry.order.requirements;
        let mut refused = None;
        let mut kept = Vec::with_capacity(candidates.len());
        for (runner, zone) in candidates {
            let Some(load) = runner.load else {
                kept.push((runner, zone));
                continue;
            };
            let mut own_cpu = 0u32;
            let mut fresh_mem = 0u64;
            for (i, slot) in slots.iter().enumerate() {
                let here = self.runners.iter().any(|r| {
                    r.name == slot.runner
                        && same_host(&r.endpoint, &runner.endpoint, &self.site.broker_addrs)
                });
                if !here {
                    continue;
                }
                let req = &slot.entry.order.requirements;
                own_cpu = own_cpu.saturating_add(req.cpu);
                if i >= running {
                    fresh_mem = fresh_mem.saturating_add(req.mem_mb);
                }
            }
            match admission.check(&load, req, own_cpu, fresh_mem) {
                Ok(()) => kept.push((runner, zone)),
                Err(w) => {
                    refused.get_or_insert(w);
                }
            }
        }
        match refused {
            Some(w) if kept.is_empty() => Err(w),
            _ => Ok(kept),
        }
    }

    fn earliest(&self, entry: &QueueEntry, slots: &[Slot<'_>]) -> Option<Timestamp> {
        if self.start_now(entry, slots).is_ok() {
            return Some(self.now);
        }
        let mut ends: Vec<Timestamp> = slots.iter().map(|s| s.end.max(self.now)).collect();
        ends.sort();
        ends.dedup();
        ends.into_iter().find(|&t| {
            let left: Vec<Slot<'_>> = slots
                .iter()
                .copied()
                .filter(|s| s.end.max(self.now) > t)
                .collect();
            self.start_now(entry, &left).is_ok()
        })
    }
}

/// Earliest moment `entry` could start given `running`, predicted from each running job's
/// start plus `timeout_s` (spec 6); `None` when it cannot start even after all of them end,
/// for example because its runners are offline.
pub fn earliest_start(
    policy: &dyn Policy,
    site: &Site,
    entry: &QueueEntry,
    runners: &[RunnerView],
    running: &[RunningJob],
    now: Timestamp,
) -> Option<Timestamp> {
    let planner = Planner {
        policy,
        site,
        runners,
        now,
    };
    let slots: Vec<Slot<'_>> = running.iter().map(slot_of).collect();
    planner.earliest(entry, &slots)
}

fn slot_of(job: &RunningJob) -> Slot<'_> {
    Slot {
        entry: &job.entry,
        runner: &job.runner,
        end: job.deadline(),
    }
}

/// One planning pass (spec 6): which queued orders start now and on which runner.
///
/// Orders are taken in the policy's dispatch order. Each starts on the most preferred free
/// suitable runner when the pool, the reserves and the quotas allow. The first blocked order
/// whose start is predictable becomes the head: its earliest start is reserved, and a later
/// order may start before it only if, with that order running until its own `timeout_s`, the
/// head could still start at the reserved time (hole filling). Running jobs are never stopped.
/// Pure: call it again on every event.
pub fn pick_next(policy: &dyn Policy, site: &Site, state: &QueueState<'_>, now: Timestamp) -> Plan {
    let planner = Planner {
        policy,
        site,
        runners: state.runners,
        now,
    };
    let mut slots: Vec<Slot<'_>> = state.running.iter().map(slot_of).collect();
    let running = slots.len();
    let mut plan = Plan::default();
    let mut head: Option<(&QueueEntry, Timestamp)> = None;
    for i in policy.order(state.queue, state.served, now) {
        let entry = &state.queue[i];
        let job = &entry.order.job;
        let end = now.plus_secs(entry.order.requirements.timeout_s);
        let startable = planner
            .start_now(entry, &slots)
            .and_then(|c| planner.admit_load(entry, c, &slots, running));
        let chosen = match startable {
            Ok(candidates) => match head {
                None => Ok(candidates[0]),
                Some((h, at)) => candidates
                    .into_iter()
                    .find(|(runner, _)| {
                        let mut with = slots.clone();
                        with.push(Slot {
                            entry,
                            runner: &runner.name,
                            end,
                        });
                        planner.earliest(h, &with).is_some_and(|t| t <= at)
                    })
                    .ok_or(Wait::HeadReserved {
                        head: h.order.job.clone(),
                        at,
                    }),
            },
            Err(wait) => {
                if head.is_none() && !matches!(wait, Wait::ForeignLoad { .. }) {
                    if let Some(at) = planner.earliest(entry, &slots) {
                        head = Some((entry, at));
                        plan.reservation = Some(Reservation {
                            job: job.clone(),
                            at,
                        });
                    }
                }
                Err(wait)
            }
        };
        match chosen {
            Ok((runner, zone)) => {
                slots.push(Slot {
                    entry,
                    runner: &runner.name,
                    end,
                });
                plan.assignments.push(Assignment {
                    job: job.clone(),
                    runner: runner.name.clone(),
                    zone,
                });
            }
            Err(wait) => plan.waiting.push((job.clone(), wait)),
        }
    }
    plan
}
