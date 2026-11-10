use super::conn::ConnId;
use super::{Client, Engine};
use crate::api::{
    access, detail_of, mask_detail, mask_summary, stats_view, summary_of, summary_of_job,
    wire_source, Access, Viewer,
};
use lr_abstractions::{JobFilter, JobKey, JobRecord, StatsQuery};
use lr_core::{JobId, JobState};
use lr_proto::{
    split_text, BrokerToHub, CapabilitiesQuery, CapabilitiesReply, HeldTag, HistoryFilter,
    JobSummary, LogChunk, PoolLoad, QueueItem, ReadAnswer, ReadCall, ReadReply, ReadRequest,
    RunnerCaps, RunnerInfo, RunningView, StatsFilter, StatusFilter, StatusView, WireSource,
    CHUNK_TEXT_LIMIT,
};

fn passes(
    project: &Option<String>,
    source: &Option<WireSource>,
    label: &Option<String>,
    s: &JobSummary,
    s_source: Option<WireSource>,
) -> bool {
    project
        .as_ref()
        .is_none_or(|p| s.project.as_ref() == Some(p))
        && label.as_ref().is_none_or(|l| s.label.as_ref() == Some(l))
        && source.is_none_or(|w| s_source == Some(w))
}

fn page<T>(items: Vec<T>, cursor: &Option<String>, limit: u64) -> (Vec<T>, Option<String>) {
    let start = cursor
        .as_deref()
        .and_then(|c| c.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = limit as usize;
    let total = items.len();
    let out: Vec<T> = items.into_iter().skip(start).take(limit).collect();
    let next = (start + limit < total).then(|| (start + limit).to_string());
    (out, next)
}

impl Engine {
    fn viewer(client: &Client) -> Viewer {
        Viewer {
            key: client.key.clone(),
            owner: client.owner,
        }
    }

    fn access_to(&self, viewer: &Viewer, run_client: &str) -> Access {
        access(
            viewer,
            run_client,
            self.policy.file().visibility.others_orders,
        )
    }

    pub(crate) async fn read(&mut self, conn: ConnId, client: &Client, call: ReadCall) {
        let viewer = Self::viewer(client);
        let reply = match call.request {
            ReadRequest::Status(f) => ReadReply::Status(self.status(&viewer, f).await),
            ReadRequest::History(f) => self.history(&viewer, f).await,
            ReadRequest::HistoryShow { job } => match self.run_of(&viewer, &job).await {
                Some((rec, a)) => {
                    let wait = self.waits.get(&rec.job.key).cloned();
                    match mask_detail(detail_of(&rec, wait), a) {
                        Some(d) => ReadReply::Job(Box::new(d)),
                        None => not_found(&job),
                    }
                }
                None => not_found(&job),
            },
            ReadRequest::Explain { job } => self.explain(&viewer, job).await,
            ReadRequest::Log { job, tail, .. } => match self.log(&viewer, &job, tail).await {
                Ok(text) => {
                    let parts = split_text(&text, CHUNK_TEXT_LIMIT);
                    let last = parts.len().saturating_sub(1);
                    if parts.is_empty() {
                        self.send(conn, &log_chunk(call.req, String::new(), true));
                    }
                    for (i, part) in parts.into_iter().enumerate() {
                        self.send(conn, &log_chunk(call.req, part.to_string(), i == last));
                    }
                    return;
                }
                Err(reply) => reply,
            },
            ReadRequest::Stats(f) => self.stats(&viewer, f).await,
            ReadRequest::Runners => ReadReply::Runners {
                runners: self.runner_infos(&viewer),
            },
            ReadRequest::Subscribe { topics } => {
                let sub = self.sub_id();
                if let Some(c) = self.conns.get_mut(&conn) {
                    c.subs.insert(sub, topics);
                }
                ReadReply::Subscribed { sub }
            }
            ReadRequest::Unsubscribe { sub } => {
                match self.conns.get_mut(&conn).and_then(|c| c.subs.remove(&sub)) {
                    Some(_) => ReadReply::Unsubscribed { sub },
                    None => not_found_text(format!("subscription {sub}")),
                }
            }
            ReadRequest::Unknown => ReadReply::Unknown,
        };
        self.send(
            conn,
            &BrokerToHub::ReadReply(ReadAnswer {
                req: call.req,
                reply,
            }),
        );
    }

    async fn status(&self, viewer: &Viewer, f: StatusFilter) -> StatusView {
        let limit = f.effective_limit() as usize;
        let mut running = Vec::new();
        for job in &self.active {
            let a = self.access_to(viewer, &job.key.client);
            let Some(summary) = mask_summary(summary_of_job(job), a) else {
                continue;
            };
            let source = a.sees_fields().then(|| source_of(job));
            if !passes(&f.project, &f.source, &f.label, &summary, source) {
                continue;
            }
            let rec = self.store.job(&job.key).await.ok().flatten();
            let steps = match (&rec, a.sees_fields()) {
                (Some(r), true) => r.steps.iter().map(crate::api::step_view).collect(),
                _ => Vec::new(),
            };
            let last_lines = if a.sees_log() {
                self.tail(&job.key, 5).await
            } else {
                Vec::new()
            };
            running.push(RunningView {
                summary,
                steps,
                last_lines,
            });
        }
        running.truncate(limit);
        let entries: Vec<_> = self.queue.iter().map(|j| j.entry.clone()).collect();
        let order = self
            .policy
            .policy()
            .order(&entries, &self.served, self.now());
        let mut queue = Vec::new();
        for (pos, i) in order.into_iter().enumerate() {
            let job = &self.queue[i];
            let a = self.access_to(viewer, &job.key.client);
            let Some(summary) = mask_summary(summary_of_job(job), a) else {
                continue;
            };
            let source = a.sees_fields().then(|| source_of(job));
            if !passes(&f.project, &f.source, &f.label, &summary, source) {
                continue;
            }
            queue.push(QueueItem {
                summary,
                position: pos as u32 + 1,
                wait_reason: a
                    .sees_fields()
                    .then(|| self.waits.get(&job.key).cloned())
                    .flatten(),
            });
        }
        let (queue, next) = page(queue, &f.cursor, limit as u64);
        let mut recent = Vec::new();
        for rec in self
            .store
            .jobs(&JobFilter::default())
            .await
            .unwrap_or_default()
        {
            if !rec.job.state.is_final() || recent.len() >= limit {
                continue;
            }
            let a = self.access_to(viewer, &rec.job.key.client);
            if let Some(s) = mask_summary(summary_of(&rec.job), a) {
                let source = a.sees_fields().then(|| wire_source(rec.job.source));
                if passes(&f.project, &f.source, &f.label, &s, source) {
                    recent.push(s);
                }
            }
        }
        let used = self.running_jobs();
        let pool = self.policy.policy().pool();
        StatusView {
            running,
            queue,
            recent,
            pool: PoolLoad {
                used_mem_mb: used.iter().map(|j| j.entry.order.requirements.mem_mb).sum(),
                total_mem_mb: pool.mem_mb,
                used_cpu: used.iter().map(|j| j.entry.order.requirements.cpu).sum(),
                total_cpu: pool.cpu,
            },
            next,
        }
    }

    async fn history(&self, viewer: &Viewer, f: HistoryFilter) -> ReadReply {
        let filter = JobFilter {
            client: f.client.clone(),
            project: f.project.clone(),
            source: f.source.map(|s| match s {
                WireSource::Session => lr_abstractions::SourceKind::Session,
                WireSource::Github => lr_abstractions::SourceKind::Github,
            }),
            label: f.label.clone(),
            profile: f.profile.clone(),
            state: f.state,
            platform: f.platform.clone(),
            runner: f.runner.clone(),
            since: f.since,
            active_only: f.active_only,
            limit: None,
        };
        let Ok(rows) = self.store.jobs(&filter).await else {
            return ReadReply::Failed {
                message: "history is unavailable".into(),
            };
        };
        let jobs: Vec<JobSummary> = rows
            .iter()
            .filter_map(|rec| {
                let a = self.access_to(viewer, &rec.job.key.client);
                let s = mask_summary(summary_of(&rec.job), a)?;
                let source = a.sees_fields().then(|| wire_source(rec.job.source));
                let profile_ok = f
                    .profile
                    .as_ref()
                    .is_none_or(|p| s.profile.as_ref() == Some(p));
                (profile_ok && passes(&f.project, &f.source, &f.label, &s, source)).then_some(s)
            })
            .collect();
        let (jobs, next) = page(jobs, &f.cursor, f.effective_limit());
        ReadReply::History { jobs, next }
    }

    async fn run_of(&self, viewer: &Viewer, job: &JobId) -> Option<(JobRecord, Access)> {
        let own = JobKey {
            client: viewer.key.clone(),
            job: job.clone(),
        };
        if let Ok(Some(rec)) = self.store.job(&own).await {
            return Some((rec, Access::Owner));
        }
        let rows = self.store.jobs(&JobFilter::default()).await.ok()?;
        rows.into_iter().find_map(|rec| {
            if &rec.job.key.job != job {
                return None;
            }
            let a = self.access_to(viewer, &rec.job.key.client);
            (a != Access::Hidden).then_some((rec, a))
        })
    }

    async fn explain(&self, viewer: &Viewer, job: JobId) -> ReadReply {
        let Some((rec, a)) = self.run_of(viewer, &job).await else {
            return not_found(&job);
        };
        let key = &rec.job.key;
        let text = match self.find(key) {
            Some(j) if j.runner.is_none() => match (a.sees_fields(), self.waits.get(key)) {
                (true, Some(w)) => format!("ждёт: {w}"),
                _ => "ждёт в очереди".to_string(),
            },
            Some(j) if j.started_at.is_none() => format!(
                "выдан раннеру {}, ждёт старта",
                j.runner.as_deref().unwrap_or_default()
            ),
            Some(j) => format!(
                "выполняется на раннере {}",
                j.runner.as_deref().unwrap_or_default()
            ),
            None => format!("завершена: {}", state_name(rec.job.state)),
        };
        ReadReply::Explain { job, text }
    }

    async fn log(
        &self,
        viewer: &Viewer,
        job: &JobId,
        tail: Option<u64>,
    ) -> Result<String, ReadReply> {
        let Some((rec, a)) = self.run_of(viewer, job).await else {
            return Err(not_found(job));
        };
        if !a.sees_log() {
            return Err(ReadReply::Denied {
                reason: "полный лог отдаётся только клиенту-владельцу запуска".into(),
            });
        }
        let bytes = self.store.read_log(&rec.job.key).await.unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        Ok(match tail {
            Some(n) => {
                let lines: Vec<&str> = text.lines().collect();
                let from = lines.len().saturating_sub(n as usize);
                lines[from..].iter().map(|l| format!("{l}\n")).collect()
            }
            None => text,
        })
    }

    async fn tail(&self, key: &JobKey, n: usize) -> Vec<String> {
        let bytes = self.store.read_log(key).await.unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let from = lines.len().saturating_sub(n);
        lines[from..].iter().map(|l| l.to_string()).collect()
    }

    async fn stats(&self, viewer: &Viewer, f: StatsFilter) -> ReadReply {
        let query = StatsQuery {
            client: if viewer.owner {
                f.client
            } else {
                Some(viewer.key.clone())
            },
            project: f.project,
            profile: f.profile,
            since: f.since,
        };
        match self.store.stats(&query).await {
            Ok(s) => ReadReply::Stats(stats_view(&s)),
            Err(e) => ReadReply::Failed {
                message: e.to_string(),
            },
        }
    }

    fn runner_infos(&self, viewer: &Viewer) -> Vec<RunnerInfo> {
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
                    caps: slot.wire.clone(),
                    last_seen: Some(slot.last_seen),
                    free_mem_mb: free_mem,
                    free_cpu,
                    held_tags,
                    services: Vec::new(),
                    unmet: Vec::new(),
                }
            })
            .collect()
    }

    pub(crate) fn capabilities(&self, conn: ConnId, client: &Client, q: CapabilitiesQuery) {
        let runners = self
            .runners
            .iter()
            .map(|(name, slot)| RunnerCaps {
                name: name.clone(),
                online: slot.conn.is_some(),
                zone: self.site.zone_between(&client.endpoint, &slot.endpoint),
                caps: slot.wire.clone(),
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

fn source_of(job: &super::Job) -> WireSource {
    match job.spec.identity.source {
        lr_core::Source::Session => WireSource::Session,
        lr_core::Source::Github(_) => WireSource::Github,
    }
}

fn state_name(s: JobState) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn not_found(job: &JobId) -> ReadReply {
    not_found_text(format!("job {job}"))
}

fn not_found_text(what: String) -> ReadReply {
    ReadReply::NotFound { what }
}

fn log_chunk(req: u64, text: String, end: bool) -> BrokerToHub {
    BrokerToHub::LogChunk(LogChunk { req, text, end })
}
