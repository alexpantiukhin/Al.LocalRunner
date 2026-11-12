use super::conn::ConnId;
use super::runners::RunnerSlot;
use super::{Client, Engine};
use crate::api::{Access, Viewer};
use lr_core::JobId;
use lr_proto::{
    BrokerToHub, CapabilitiesQuery, CapabilitiesReply, HeldTag, RunnerCaps, RunnerInfo,
    WireCapabilities,
};

fn caps_for(viewer: &Viewer, slot: &RunnerSlot) -> WireCapabilities {
    let mut caps = slot.wire.clone();
    let own = caps.machine.as_ref().is_some_and(|m| m.0 == viewer.key);
    if !viewer.owner && !own {
        caps.machine = None;
    }
    caps
}

impl Engine {
    pub(crate) fn runner_infos(&self, viewer: &Viewer) -> Vec<RunnerInfo> {
        let running = self.running_jobs();
        let pool = self.policy.policy().pool();
        let free_mem = pool.mem_mb.saturating_sub(
            running
                .iter()
                .map(|j| j.entry.order.requirements.mem_mb)
                .sum(),
        );
        let free_cpu = pool
            .cpu
            .saturating_sub(running.iter().map(|j| j.entry.order.requirements.cpu).sum());
        self.runners
            .iter()
            .map(|(name, slot)| {
                let mut held_tags = Vec::new();
                for job in self
                    .active
                    .iter()
                    .filter(|j| j.runner.as_deref() == Some(name))
                {
                    let req = &job.entry.order.requirements;
                    let shown = match self.access_to(viewer, &job.key.client) {
                        Access::Hidden => JobId(String::new()),
                        _ => job.key.job.clone(),
                    };
                    for tag in req.tags.iter().chain(req.exclusive.iter()) {
                        held_tags.push(HeldTag {
                            tag: tag.clone(),
                            job: shown.clone(),
                            exclusive: req.exclusive.contains(tag)
                                || pool.exclusive_tags.contains(tag),
                        });
                    }
                }
                RunnerInfo {
                    name: name.clone(),
                    online: slot.conn.is_some(),
                    caps: caps_for(viewer, slot),
                    last_seen: Some(slot.last_seen),
                    free_mem_mb: free_mem,
                    free_cpu,
                    held_tags,
                    services: slot.services.clone(),
                    unmet: Vec::new(),
                }
            })
            .collect()
    }

    pub(crate) fn capabilities(&self, conn: ConnId, client: &Client, q: CapabilitiesQuery) {
        let viewer = Self::viewer(client);
        let runners = self
            .runners
            .iter()
            .map(|(name, slot)| RunnerCaps {
                name: name.clone(),
                online: slot.conn.is_some(),
                zone: self.site.zone_between(&client.endpoint, &slot.endpoint),
                caps: caps_for(&viewer, slot),
            })
            .collect();
        self.send(
            conn,
            &BrokerToHub::CapabilitiesReply(CapabilitiesReply {
                req: q.req,
                zone: self.zone_of_peer(&client.endpoint),
                runners,
            }),
        );
    }
}
