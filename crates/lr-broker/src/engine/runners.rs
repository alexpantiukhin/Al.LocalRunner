use super::conn::ConnId;
use super::recovery::unknown_address;
use super::{Engine, Job, Role};
use lr_abstractions::{Level, RunnerRow, RunnerState, StoreError};
use lr_core::{Capabilities, Endpoint, MachineRef, RunnerView, Timestamp, Zone};
use lr_proto::{
    ProtocolError, ProtocolErrorCode, QueueEvent, ServiceReport, WireCapabilities, WireLoad,
    WireServiceStatus,
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
    pub(crate) draining: bool,
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
            draining: false,
            services: Vec::new(),
            stopping: BTreeSet::new(),
            restored: true,
            load: None,
        }
    }

    pub(crate) fn takes_work(&self) -> bool {
        self.conn.is_some() && !self.draining
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
                draining: false,
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
        self.runners
            .iter()
            .map(|(name, s)| RunnerView {
                name: name.clone(),
                caps: s.caps.clone(),
                endpoint: s.endpoint.clone(),
                online: s.takes_work(),
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
        slot.last_seen = now;
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
        if load_changed && on_foreign_load {
            self.plan().await;
        }
    }

    pub(crate) async fn runner_gone(&mut self, name: &str) {
        let now = self.now();
        let Some(slot) = self.runners.get_mut(name) else {
            return;
        };
        if slot.conn.take().is_none() {
            return;
        }
        slot.load = None;
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
