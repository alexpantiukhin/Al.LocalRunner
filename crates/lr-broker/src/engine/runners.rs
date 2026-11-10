use super::conn::ConnId;
use super::{Engine, Role};
use lr_abstractions::{RunnerRow, RunnerState, StoreError};
use lr_core::{Capabilities, Endpoint, MachineRef, RunnerView, Timestamp, Zone};
use lr_proto::{ProtocolError, ProtocolErrorCode, QueueEvent, WireCapabilities, WireServiceStatus};

pub(crate) struct RunnerSlot {
    pub(crate) owner: String,
    pub(crate) persisted: bool,
    pub(crate) wire: WireCapabilities,
    pub(crate) caps: Capabilities,
    pub(crate) endpoint: Endpoint,
    pub(crate) conn: Option<ConnId>,
    pub(crate) last_seen: Timestamp,
}

fn row_of(name: &str, owner: &str, wire: &WireCapabilities, at: Timestamp) -> RunnerRow {
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
        if let Some(old) = self.runners.get(name).and_then(|s| s.conn) {
            if old != conn {
                if let Some(c) = self.conns.get_mut(&old) {
                    c.role = Role::Pending;
                }
                self.close(old);
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
            },
        );
        if let Some(c) = self.conns.get_mut(&conn) {
            c.role = Role::Runner(name.to_string());
        }
        self.publish(
            None,
            QueueEvent::RunnerOnline {
                name: name.to_string(),
            },
        );
        Ok(())
    }

    pub(crate) fn runner_views(&self) -> Vec<RunnerView> {
        self.runners
            .iter()
            .map(|(name, s)| RunnerView {
                name: name.clone(),
                caps: s.caps.clone(),
                endpoint: s.endpoint.clone(),
                online: s.conn.is_some(),
            })
            .collect()
    }

    pub(crate) fn runner_conn(&self, name: &str) -> Option<ConnId> {
        self.runners.get(name).and_then(|s| s.conn)
    }

    pub(crate) async fn runner_heartbeat(&mut self, name: &str) {
        let now = self.now();
        let Some(slot) = self.runners.get_mut(name) else {
            return;
        };
        slot.last_seen = now;
        if slot.persisted {
            let _ = self
                .store
                .set_runner_state(name, RunnerState::Online, now)
                .await;
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
        if slot.persisted {
            let _ = self
                .store
                .set_runner_state(name, RunnerState::Offline, now)
                .await;
        }
        self.publish(
            None,
            QueueEvent::RunnerOffline {
                name: name.to_string(),
            },
        );
        let (back, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.active)
            .into_iter()
            .partition(|j| j.runner.as_deref() == Some(name) && j.started_at.is_none());
        self.active = keep;
        for mut job in back {
            if self.store.requeue(&job.key).await.is_err() {
                continue;
            }
            job.runner = None;
            match job.cancel.take() {
                Some(cancel) => self.cancel_waiting(job, cancel).await,
                None => self.queue.push(job),
            }
        }
        self.plan().await;
    }
}
