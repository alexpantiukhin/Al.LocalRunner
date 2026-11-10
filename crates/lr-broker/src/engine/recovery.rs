use super::local::LOCAL_RUNNER_PREFIX;
use super::runners::RunnerSlot;
use super::{Cancel, Client, Engine, Job};
use lr_abstractions::{JobFilter, JobRecord, Level, RunnerRow, RunnerState};
use lr_core::{ClientView, Declared, Endpoint, JobState, MachineRef, QueueEntry};
use lr_proto::{OrderSpec, ReasonCode, WireCapabilities, WireServiceStatus};
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
        for rec in records {
            self.restore(rec).await;
        }
    }

    async fn restore(&mut self, rec: JobRecord) {
        let Ok(spec) = serde_json::from_str::<OrderSpec>(&rec.spec.resolved_json) else {
            return self.diag(
                Level::Error,
                "stored order spec is unreadable, the run is left as it is",
                &[("client", &rec.job.key.client), ("job", &rec.job.key.job.0)],
            );
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
        let mut job = Job {
            key: key.clone(),
            internal: entry.order.job.clone(),
            entry,
            spec,
            runner: rec.job.runner.clone(),
            assigned_at: now,
            started_at: rec.job.started_at,
            cancel: None,
        };
        if job.started_at.is_none() {
            if job.runner.is_some() && self.store.requeue(&key).await.is_err() {
                return;
            }
            job.runner = None;
            self.queue.push(job);
            return;
        }
        if rec.job.state == JobState::Cancelling {
            let reason = rec.job.cancel_reason.clone().unwrap_or_default();
            let code = reason
                .split(':')
                .next()
                .and_then(|c| c.trim().parse::<ReasonCode>().ok())
                .unwrap_or(ReasonCode::CancelledByUser);
            job.cancel = Some(Cancel {
                by: rec.job.cancelled_by.clone().unwrap_or_default(),
                reason,
                code,
                detail: None,
                sent: false,
            });
        }
        self.active.push(job);
    }

    pub(crate) fn refresh_client(&mut self, client: &Client) {
        self.client_seen.insert(client.key.clone(), self.now());
        for job in self.queue.iter_mut().filter(|j| j.key.client == client.key) {
            job.entry.client.endpoint = client.endpoint.clone();
            job.entry.client.label = client.label.clone();
        }
    }
}
