use super::conn::ConnId;
use super::recovery::unknown_address;
use super::{Engine, Job, Role};
use lr_abstractions::{Level, RunnerRow, RunnerState, StoreError};
use lr_core::{Capabilities, Endpoint, MachineRef, RunnerView, Timestamp, Zone};
use lr_proto::{
    DrainReason, ProtocolError, ProtocolErrorCode, QueueEvent, RunnerLiveness, ServiceReport,
    WireCapabilities, WireLoad, WireServiceStatus, HEARTBEAT_INTERVAL_S, MISSED_HEARTBEATS_OFFLINE,
    STALE_AFTER_MISSED,
};
use std::collections::BTreeSet;

pub(crate) struct RunnerSlot {
    pub(crate) owner: String,
    pub(crate) persisted: bool,
    pub(crate) wire: WireCapabilities,
    pub(crate) caps: Capabilities,
    pub(crate) endpoint: Endpoint,
    pub(crate) conn: Option<ConnId>,
    pub(crate) last_seen: Timestamp,
    pub(crate) last_heard: Timestamp,
    pub(crate) connected_once: bool,
    pub(crate) heartbeat_proven: bool,
    pub(crate) draining: bool,
    pub(crate) drain_reason: Option<DrainReason>,
    pub(crate) hold_until: Option<Timestamp>,
    pub(crate) services: Vec<ServiceReport>,
    pub(crate) stopping: BTreeSet<String>,
    pub(crate) restored: bool,
    pub(crate) load: Option<WireLoad>,
}

impl RunnerSlot {
    pub(crate) fn offline(row: &RunnerRow, wire: WireCapabilities) -> RunnerSlot {
        RunnerSlot {
            owner: row.machine_name.clone(),
            persisted: true,
            caps: wire.clone().into_capabilities(Zone::Remote),
            wire,
            endpoint: unknown_address(),
            conn: None,
            last_seen: row.last_seen.unwrap_or(Timestamp(0)),
            last_heard: row.last_seen.unwrap_or(Timestamp(0)),
            connected_once: false,
            heartbeat_proven: false,
            draining: false,
            drain_reason: None,
            hold_until: None,
            services: Vec::new(),
            stopping: BTreeSet::new(),
            restored: true,
            load: None,
        }
    }

    pub(crate) fn takes_work(&self) -> bool {
        self.conn.is_some() && !self.draining
    }

    pub(crate) fn liveness(&self, now: Timestamp) -> RunnerLiveness {
        if self.conn.is_none() {
            return RunnerLiveness::Offline;
        }
        let silent_ms = now.millis_since(self.last_heard);
        let interval_ms = HEARTBEAT_INTERVAL_S * 1000;
        let stale_ms = interval_ms * u64::from(STALE_AFTER_MISSED) + interval_ms / 2;
        let offline_ms = interval_ms * u64::from(MISSED_HEARTBEATS_OFFLINE);
        if silent_ms < stale_ms {
            RunnerLiveness::Online
        } else if silent_ms < offline_ms {
            RunnerLiveness::Stale
        } else {
            RunnerLiveness::Offline
        }
    }

    pub(crate) fn serves(&self, now: Timestamp) -> bool {
        self.takes_work() && self.liveness(now) == RunnerLiveness::Online
    }

    pub(crate) fn drains_for_upgrade(&self) -> bool {
        self.draining
            && matches!(
                self.drain_reason,
                Some(DrainReason::Upgrade | DrainReason::Lock)
            )
    }

    pub(crate) fn holds_orders(&self, now: Timestamp) -> bool {
        self.drains_for_upgrade()
            && (self.conn.is_some() || self.hold_until.is_some_and(|until| now < until))
    }
}

pub(crate) fn row_of(name: &str, owner: &str, wire: &WireCapabilities, at: Timestamp) -> RunnerRow {
    RunnerRow {
        name: name.to_string(),
        machine_name: owner.to_string(),
        programs: wire.programs.clone(),
        platforms: wire.platforms.clone(),
        tags: wire.tags.clone(),
        max_timeout_s: wire.max_job_timeout_s,
        cache_max_mb: wire.cache_max_mb.unwrap_or(0),
        cache_used_mb: wire.cache_used_mb.unwrap_or(0),
        free_disk_mb: wire.disk_mb,
        services: wire
            .services
            .iter()
            .map(|(n, s)| (n.clone(), *s == WireServiceStatus::Available))
            .collect(),
        accept_min_mem_mb: wire.accept_min_mem_mb,
        enforcement: wire.enforcement.clone(),
        version: wire.version.clone(),
        last_seen: Some(at),
        state: RunnerState::Online,
    }
}

fn name_taken(name: &str) -> ProtocolError {
    ProtocolError {
        code: ProtocolErrorCode::NameTaken,
        message: format!("runner name {name} belongs to another machine"),
        supported: None,
    }
}

impl Engine {
    pub(crate) async fn register_runner(
        &mut self,
        conn: ConnId,
        name: &str,
        mut wire: WireCapabilities,
        owner: String,
        persisted: bool,
        machine: Option<String>,
    ) -> Result<(), ProtocolError> {
        if self.runners.get(name).is_some_and(|s| s.owner != owner) {
            return Err(name_taken(name));
        }
        let first_contact = self.runners.get(name).is_some_and(|s| s.restored);
        if let Some(old) = self.runners.get(name).and_then(|s| s.conn) {
            if old != conn {
                if let Some(c) = self.conns.get_mut(&old) {
                    c.role = Role::Pending;
                }
                self.close(old);
                self.runner_gone(name).await;
            }
        }
        let now = self.now();
        let last_heard = match self.runners.get(name) {
            Some(prev) if prev.connected_once && !prev.heartbeat_proven => prev.last_heard,
            _ => now,
        };
        wire.machine = machine.map(MachineRef);
        if persisted {
            if let Err(StoreError::Duplicate(_)) = self
                .store
                .upsert_runner(row_of(name, &owner, &wire, now))
                .await
            {
                return Err(name_taken(name));
            }
        }
        let endpoint = self
            .conns
            .get(&conn)
            .map(|c| c.peer.endpoint.clone())
            .unwrap_or(Endpoint::Ipc {
                os_user: String::new(),
            });
        self.runners.insert(
            name.to_string(),
            RunnerSlot {
                owner,
                persisted,
                caps: wire.clone().into_capabilities(Zone::Remote),
                wire,
                endpoint,
                conn: Some(conn),
                last_seen: now,
                last_heard,
                connected_once: true,
                heartbeat_proven: false,
                draining: false,
                drain_reason: None,
                hold_until: None,
                services: Vec::new(),
                stopping: BTreeSet::new(),
                restored: false,
                load: None,
            },
        );
        if let Some(c) = self.conns.get_mut(&conn) {
            c.role = Role::Runner(name.to_string());
        }
        if first_contact {
            for job in self.active.iter_mut() {
                if job.runner.as_deref() == Some(name) && job.lost_floor.is_some() {
                    job.lost_floor = Some(now);
                }
            }
        }
        self.publish(
            None,
            QueueEvent::RunnerOnline {
                name: name.to_string(),
            },
        );
        Ok(())
    }

    pub(crate) async fn runner_resumed(&mut self, runner: &str, points: &[lr_proto::ResumePoint]) {
        let held = |j: &Job| {
            j.runner.as_deref() == Some(runner) && j.started_at.is_none() && j.gone_since.is_some()
        };
        let (unknown, keep): (Vec<Job>, Vec<Job>) = std::mem::take(&mut self.active)
            .into_iter()
            .partition(|j| held(j) && !points.iter().any(|p| p.job == j.key.job));
        self.active = keep;
        for j in self.active.iter_mut().filter(|j| held(j)) {
            j.gone_since = None;
        }
        let changed = !unknown.is_empty();
        for job in unknown {
            self.requeue_or_retry(job).await;
        }
        if changed {
            self.plan().await;
        }
    }

    pub(crate) fn confirm_assign(&mut self, runner: &str, job: &lr_core::JobId) {
        if let Some(j) = self
            .active
            .iter_mut()
            .find(|j| j.runner.as_deref() == Some(runner) && &j.key.job == job)
        {
            j.confirmed = true;
        }
    }

    pub(crate) async fn resend_cancels(&mut self, name: &str) {
        let pending: Vec<usize> = (0..self.active.len())
            .filter(|&i| {
                let j = &self.active[i];
                j.runner.as_deref() == Some(name) && j.cancel.as_ref().is_some_and(|c| !c.sent)
            })
            .collect();
        for i in pending {
            self.send_cancel(i).await;
        }
    }

    pub(crate) fn runner_views(&self) -> Vec<RunnerView> {
        let now = self.now();
        self.runners
            .iter()
            .map(|(name, s)| RunnerView {
                name: name.clone(),
                caps: s.caps.clone(),
                endpoint: s.endpoint.clone(),
                online: s.serves(now) || s.holds_orders(now),
                load: s.load.map(Into::into),
            })
            .collect()
    }

    pub(crate) fn runner_conn(&self, name: &str) -> Option<ConnId> {
        self.runners.get(name).and_then(|s| s.conn)
    }

    pub(crate) async fn runner_heartbeat(&mut self, name: &str, load: Option<WireLoad>) {
        let now = self.now();
        let Some(slot) = self.runners.get_mut(name) else {
            return;
        };
        let was_online = slot.liveness(now) == RunnerLiveness::Online;
        slot.last_seen = now;
        slot.last_heard = now;
        slot.heartbeat_proven = true;
        let load_changed = load.is_some() && slot.load != load;
        if load.is_some() {
            slot.load = load;
        }
        let state = if slot.draining {
            RunnerState::Draining
        } else {
            RunnerState::Online
        };
        if slot.persisted {
            if let Err(e) = self.store.set_runner_state(name, state, now).await {
                self.diag(
                    Level::Error,
                    "storage failed: could not record the runner heartbeat",
                    &[("runner", name), ("error", &e.to_string())],
                );
            }
        }
        let on_foreign_load = self.wait_codes.values().any(|c| *c == "foreign_load");
        if !was_online || (load_changed && on_foreign_load) {
            self.plan().await;
        }
    }

    pub(crate) async fn runner_gone(&mut self, name: &str) {
        let now = self.now();
        let hold_s = self.upgrade_hold_s();
        let Some(slot) = self.runners.get_mut(name) else {
            return;
        };
        if slot.conn.take().is_none() {
            return;
        }
        slot.load = None;
        if slot.drains_for_upgrade() {
            slot.hold_until = Some(now.plus_secs(hold_s));
        }
        if slot.persisted {
            if let Err(e) = self
                .store
                .set_runner_state(name, RunnerState::Offline, now)
                .await
            {
                self.diag(
                    Level::Error,
                    "storage failed: could not record the runner going offline",
                    &[("runner", name), ("error", &e.to_string())],
                );
            }
        }
        self.publish(
            None,
            QueueEvent::RunnerOffline {
                name: name.to_string(),
            },
        );
        let (back, keep): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.active).into_iter().partition(|j| {
                j.runner.as_deref() == Some(name) && j.started_at.is_none() && !j.confirmed
            });
        self.active = keep;
        for job in self.active.iter_mut() {
            if job.runner.as_deref() == Some(name) {
                if let Some(c) = job.cancel.as_mut() {
                    c.sent = false;
                }
                if job.started_at.is_none() && job.gone_since.is_none() {
                    job.gone_since = Some(now);
                }
            }
        }
        for job in back {
            self.requeue_or_retry(job).await;
        }
        self.return_stranded().await;
        self.plan().await;
    }
}
