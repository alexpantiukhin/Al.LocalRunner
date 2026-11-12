use super::local::LOCAL_RUNNER_PREFIX;
use super::runners::RunnerSlot;
use super::{Cancel, Client, Engine, Job};
use lr_abstractions::{JobFilter, JobKey, JobRecord, Level, MachineRow, RunnerRow, RunnerState};
use lr_core::{ClientView, Declared, Endpoint, JobState, MachineRef, QueueEntry};
use lr_proto::{OrderSpec, ReasonCode, WireCapabilities, WireServiceStatus};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};

const RECOVERY_SCAN: usize = 100_000;

pub(crate) fn unknown_address() -> Endpoint {
    Endpoint::Addr(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
}

pub(crate) fn client_endpoint(key: &str) -> Endpoint {
    match key.strip_prefix(LOCAL_RUNNER_PREFIX) {
        Some(os_user) => Endpoint::Ipc {
            os_user: os_user.to_string(),
        },
        None => unknown_address(),
    }
}

fn stored_cancel(rec: &JobRecord) -> Cancel {
    let reason = rec.job.cancel_reason.clone().unwrap_or_default();
    let (code, detail) = match reason.split_once(':') {
        Some((code, detail)) => (code, Some(detail.trim().to_string())),
        None => (reason.as_str(), None),
    };
    Cancel {
        by: rec.job.cancelled_by.clone().unwrap_or_default(),
        code: code
            .trim()
            .parse::<ReasonCode>()
            .unwrap_or(ReasonCode::CancelledByUser),
        detail,
        reason,
        sent: false,
    }
}

fn of_revoked_key(rec: &JobRecord, machines: &HashMap<String, MachineRow>) -> bool {
    machines
        .get(&rec.job.key.client)
        .is_some_and(|m| m.revoked_at.is_some() || rec.job.queued_at < m.registered_at)
}

impl Engine {
    fn wire_of(&self, row: &RunnerRow) -> WireCapabilities {
        let pool = self.policy.policy().pool();
        WireCapabilities {
            platforms: row.platforms.clone(),
            mem_mb: pool.mem_mb,
            cpu: pool.cpu,
            disk_mb: row.free_disk_mb,
            programs: row.programs.clone(),
            tags: row.tags.clone(),
            max_job_timeout_s: row.max_timeout_s,
            services: row
                .services
                .iter()
                .map(|(name, ok)| {
                    let status = if *ok {
                        WireServiceStatus::Available
                    } else {
                        WireServiceStatus::Unavailable
                    };
                    (name.clone(), status)
                })
                .collect(),
            machine: None,
            accept_min_mem_mb: row.accept_min_mem_mb,
            cache_max_mb: Some(row.cache_max_mb),
            cache_used_mb: Some(row.cache_used_mb),
            service_mem_mb: Default::default(),
            commands: Default::default(),
            enforcement: row.enforcement.clone(),
        }
    }

    pub(crate) async fn recover(&mut self) {
        self.booted_at = self.now();
        match self.store.runners().await {
            Ok(rows) => {
                for row in rows.iter().filter(|r| r.state != RunnerState::Removed) {
                    let wire = self.wire_of(row);
                    self.runners
                        .insert(row.name.clone(), RunnerSlot::offline(row, wire));
                }
            }
            Err(e) => self.diag(
                Level::Error,
                "storage failed: could not load runners at start",
                &[("error", &e.to_string())],
            ),
        }
        let machines: HashMap<String, MachineRow> = match self.store.machines().await {
            Ok(rows) => rows.into_iter().map(|m| (m.name.clone(), m)).collect(),
            Err(e) => {
                self.diag(
                    Level::Error,
                    "storage failed: could not load revoked keys at start",
                    &[("error", &e.to_string())],
                );
                HashMap::new()
            }
        };
        let filter = JobFilter {
            active_only: true,
            limit: Some(RECOVERY_SCAN),
            ..JobFilter::default()
        };
        let mut records = match self.store.jobs(&filter).await {
            Ok(rows) => rows,
            Err(e) => {
                return self.diag(
                    Level::Error,
                    "storage failed: could not load the queue at start",
                    &[("error", &e.to_string())],
                )
            }
        };
        if records.len() >= RECOVERY_SCAN {
            self.diag(
                Level::Warn,
                "more active runs than the recovery scan holds; older ones stay unplanned",
                &[("limit", &RECOVERY_SCAN.to_string())],
            );
        }
        records.sort_by(|a, b| {
            (a.job.queued_at, &a.job.key.client, &a.job.key.job.0).cmp(&(
                b.job.queued_at,
                &b.job.key.client,
                &b.job.key.job.0,
            ))
        });
        self.restored = records.iter().any(|r| r.job.started_at.is_none());
        let retired: HashSet<JobKey> = records
            .iter()
            .filter(|r| of_revoked_key(r, &machines))
            .map(|r| r.job.key.clone())
            .collect();
        self.silenced.extend(retired.iter().cloned());
        for rec in records {
            self.restore(rec).await;
        }
        self.retire(retired).await;
    }

    async fn restore(&mut self, rec: JobRecord) {
        let Some(mut job) = self.stored_job(&rec) else {
            return;
        };
        let cancelled = rec.job.state == JobState::Cancelling || rec.job.cancel_reason.is_some();
        if cancelled {
            job.cancel = Some(stored_cancel(&rec));
        }
        if job.started_at.is_some() {
            return self.active.push(job);
        }
        match (job.runner.is_some(), job.cancel.take()) {
            (_, Some(cancel)) => self.cancel_waiting(job, cancel).await,
            (true, None) => {
                job.confirmed = true;
                job.gone_since = Some(self.now());
                self.active.push(job);
            }
            (false, None) => self.queue.push(job),
        }
    }

    pub(crate) fn stored_job(&mut self, rec: &JobRecord) -> Option<Job> {
        let Ok(spec) = serde_json::from_str::<OrderSpec>(&rec.spec.resolved_json) else {
            self.diag(
                Level::Error,
                "stored order spec is unreadable, the run is left as it is",
                &[("client", &rec.job.key.client), ("job", &rec.job.key.job.0)],
            );
            return None;
        };
        let key = rec.job.key.clone();
        let mut requirements = spec.requirements.clone();
        if requirements.same_machine.is_some() {
            requirements.same_machine = Some(MachineRef(key.client.clone()));
        }
        let now = self.now();
        let entry = QueueEntry {
            order: lr_core::Order {
                job: self.internal_id(),
                requirements,
                enqueued_at: spec.enqueued_at,
                declared: Declared::from_identity(&spec.identity),
                wanted_priority: spec.wanted_priority,
            },
            client: ClientView {
                key: key.client.clone(),
                label: rec.job.client_label.clone(),
                machine_id: rec.job.client_machine.clone(),
                endpoint: client_endpoint(&key.client),
            },
            accepted_at: now,
        };
        Some(Job {
            key: key.clone(),
            internal: entry.order.job.clone(),
            entry,
            spec,
            runner: rec.job.runner.clone(),
            assigned_at: now,
            started_at: rec.job.started_at,
            cancel: None,
            start_unsaved: false,
            queued_since: now,
            platform_label: None,
            confirmed: false,
            gone_since: None,
            lost_floor: rec.job.started_at.map(|_| now),
        })
    }

    pub(crate) fn refresh_client(&mut self, client: &Client) {
        self.client_seen.insert(client.key.clone(), self.now());
        for job in self.queue.iter_mut().filter(|j| j.key.client == client.key) {
            job.entry.client.endpoint = client.endpoint.clone();
            job.entry.client.label = client.label.clone();
        }
    }
}
