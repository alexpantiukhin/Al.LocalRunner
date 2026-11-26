use super::conn::ConnId;
use super::runners::RunnerSlot;
use super::{Client, Engine};
use crate::api::Viewer;
use lr_core::{JobId, Requirements, Scheduler, Timestamp};
use lr_proto::{
    BrokerToHub, CapabilitiesQuery, CapabilitiesReply, HeldTag, OrderSpec, PoolInfo, RunnerCaps,
    RunnerInfo, RunnerKind, RunnerLiveness, WireCapabilities,
};
use std::collections::BTreeSet;

fn caps_for(viewer: &Viewer, slot: &RunnerSlot) -> WireCapabilities {
    let mut caps = slot.wire.clone();
    let own = caps.machine.as_ref().is_some_and(|m| m.0 == viewer.key);
    if !viewer.owner && !own {
        caps.machine = None;
    }
    caps
}

fn least_order() -> Requirements {
    Requirements {
        platform: None,
        timeout_s: 1,
        mem_mb: 1,
        cpu: 1,
        disk_mb: 0,
        programs: Vec::new(),
        tags: BTreeSet::new(),
        exclusive: BTreeSet::new(),
        services: Vec::new(),
        secrets: BTreeSet::new(),
        cache: Vec::new(),
        zones: None,
        commands: BTreeSet::new(),
    }
}

impl Engine {
    fn pool_free(&self) -> (u64, u32) {
        let running = self.running_jobs();
        let pool = self.policy.policy().pool();
        let mem = pool.mem_mb.saturating_sub(
            running
                .iter()
                .map(|j| j.entry.order.requirements.mem_mb)
                .sum(),
        );
        let cpu = pool
            .cpu
            .saturating_sub(running.iter().map(|j| j.entry.order.requirements.cpu).sum());
        (mem, cpu)
    }

    fn pool_has_room(&self) -> bool {
        let mut sched = Scheduler::new(self.policy.policy().pool().clone());
        for job in self.running_jobs() {
            sched.insert_running(&job.entry.order.job, &job.entry.order.requirements);
        }
        sched
            .can_grant(&JobId(String::new()), &least_order())
            .is_ok()
    }

    fn held_tags(&self, viewer: &Viewer, runner: Option<&str>) -> Vec<HeldTag> {
        let pool = self.policy.policy().pool();
        let mut held = Vec::new();
        for job in self
            .active
            .iter()
            .filter(|j| runner.is_none_or(|r| j.runner.as_deref() == Some(r)))
        {
            let req = &job.entry.order.requirements;
            let shown = if self.access_to(viewer, &job.key.client).sees_fields() {
                job.key.job.clone()
            } else {
                JobId(String::new())
            };
            for tag in req.tags.iter().chain(req.exclusive.iter()) {
                held.push(HeldTag {
                    tag: tag.clone(),
                    job: shown.clone(),
                    exclusive: req.exclusive.contains(tag) || pool.exclusive_tags.contains(tag),
                });
            }
        }
        held
    }

    pub(crate) fn runner_infos(&self, viewer: &Viewer) -> Vec<RunnerInfo> {
        let now = self.now();
        let (free_mem, free_cpu) = self.pool_free();
        let pool_name = self.policy.file().pool.display_name().to_string();
        let room = self.pool_has_room();
        self.runners
            .iter()
            .map(|(name, slot)| {
                let liveness = slot.liveness(now);
                let busy = self
                    .active
                    .iter()
                    .any(|j| j.runner.as_deref() == Some(name.as_str()));
                RunnerInfo {
                    name: name.clone(),
                    online: liveness != RunnerLiveness::Offline,
                    caps: {
                        let mut caps = caps_for(viewer, slot);
                        if let Some(free) = slot.load.and_then(|l| l.free_disk_mb) {
                            caps.disk_mb = free;
                        }
                        caps
                    },
                    last_seen: Some(slot.last_seen),
                    free_mem_mb: free_mem,
                    free_cpu,
                    held_tags: self.held_tags(viewer, Some(name)),
                    services: slot.services.clone(),
                    unmet: Vec::new(),
                    liveness: Some(liveness),
                    silent_for_s: Some(now.millis_since(slot.last_heard) / 1000),
                    kind: Some(if slot.persisted {
                        RunnerKind::Joined
                    } else {
                        RunnerKind::Local
                    }),
                    pool: Some(pool_name.clone()),
                    can_start_now: Some(
                        slot.takes_work() && liveness == RunnerLiveness::Online && !busy && room,
                    ),
                    busy: Some(busy),
                }
            })
            .collect()
    }

    pub(crate) fn pool_infos(&self, viewer: &Viewer) -> Vec<PoolInfo> {
        let pool = self.policy.policy().pool();
        let (mem_free_mb, cpu_free) = self.pool_free();
        vec![PoolInfo {
            name: self.policy.file().pool.display_name().to_string(),
            mem_total_mb: pool.mem_mb,
            mem_free_mb,
            cpu_total: pool.cpu,
            cpu_free,
            held_tags: self.held_tags(viewer, None),
            runners: self.runners.keys().cloned().collect(),
        }]
    }

    /// Whether the planning pass would assign `spec` of `client` to a runner at once, when it
    /// joined the queue now.
    fn would_start_now(&self, client: &Client, mut spec: OrderSpec, now: Timestamp) -> bool {
        spec.requirements.commands = self.command_names(&client.key, &client.label, &spec);
        let id = JobId("#probe".into());
        let entry = self.queue_entry(client, &spec, id.clone());
        self.plan_holding_with(now, self.service_mem_mb(), Some(entry))
            .assignments
            .iter()
            .any(|a| a.job == id)
    }

    pub(crate) fn capabilities(&self, conn: ConnId, client: &Client, q: CapabilitiesQuery) {
        let viewer = Self::viewer(client);
        let now = self.now();
        let start_now = q.probe.map(|spec| self.would_start_now(client, *spec, now));
        let runners = self
            .runners
            .iter()
            .map(|(name, slot)| RunnerCaps {
                name: name.clone(),
                online: slot.liveness(now) == RunnerLiveness::Online,
                zone: self
                    .site
                    .zone_between(&client.endpoint, &client.label, &slot.endpoint, name),
                caps: caps_for(&viewer, slot),
            })
            .collect();
        self.send(
            conn,
            &BrokerToHub::CapabilitiesReply(CapabilitiesReply {
                req: q.req,
                zone: self.zone_of_peer(&client.endpoint),
                runners,
                start_now,
            }),
        );
    }
}
