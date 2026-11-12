use super::conn::ConnId;
use super::{Client, Engine};
use crate::api::{
    access, detail_of, mask_detail, mask_summary, peaks_text, record_summary, stats_view,
    summary_of_job, wire_source, Access, Viewer,
};
use crate::MAX_SUBSCRIPTIONS;
use lr_abstractions::{Frame, JobFilter, JobKey, JobRecord, JobRow, StatsQuery};
use lr_core::{JobId, JobState};
use lr_proto::{
    split_text, BrokerToHub, JobSummary, LogChunk, PoolLoad, QueueItem, ReadAnswer, ReadCall,
    ReadReply, ReadRequest, RunningView, StatsFilter, StatusFilter, StatusView, WireSource,
    CHUNK_TEXT_LIMIT,
};

pub(super) const SCAN_LIMIT: usize = 10_000;
const TAIL_BYTES: u64 = 64 * 1024;
const HEADER: &str = "=== step ";

pub(super) fn passes(
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

pub(super) fn page<T>(
    items: Vec<T>,
    cursor: &Option<String>,
    limit: u64,
) -> (Vec<T>, Option<String>) {
    let start = cursor
        .as_deref()
        .and_then(|c| c.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let total = items.len();
    let out: Vec<T> = items.into_iter().skip(start).take(limit).collect();
    let end = start.saturating_add(limit);
    let next = (end < total).then(|| end.to_string());
    (out, next)
}

fn header_of(line: &str, step: &str) -> Option<bool> {
    let rest = line.strip_prefix(HEADER)?;
    if !line.trim_end().ends_with("===") {
        return None;
    }
    let after = rest.strip_prefix(step);
    Some(after.is_some_and(|a| {
        a.starts_with(" |") || a.starts_with(" ===") || a.starts_with(" cancelled at ")
    }))
}

fn step_section(text: &str, step: &str) -> String {
    let mut inside = false;
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        if let Some(mine) = header_of(line, step) {
            inside = mine;
        }
        if inside {
            out.push_str(line);
        }
    }
    out
}

fn last_lines(text: &str, n: usize) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    let from = lines.len().saturating_sub(n);
    lines[from..].to_vec()
}

impl Engine {
    pub(crate) fn viewer(client: &Client) -> Viewer {
        Viewer {
            key: client.key.clone(),
            owner: client.owner,
        }
    }

    pub(crate) fn access_to(&self, viewer: &Viewer, run_client: &str) -> Access {
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
            ReadRequest::HistoryDetails(f) => self.history_details(&viewer, f).await,
            ReadRequest::HistoryShow { job, client } => {
                match self.run_of(&viewer, &job, client.as_deref()).await {
                    Ok((rec, a)) => {
                        let wait = self.waits.get(&rec.job.key).cloned();
                        match mask_detail(detail_of(&rec, wait), a) {
                            Some(d) => ReadReply::Job(Box::new(d)),
                            None => not_found(&job),
                        }
                    }
                    Err(reply) => reply,
                }
            }
            ReadRequest::Explain { job, client } => {
                self.explain(&viewer, job, client.as_deref()).await
            }
            ReadRequest::Log {
                job,
                step,
                tail,
                client,
                follow: true,
            } => match self
                .follow_log(conn, &viewer, call.req, &job, client.as_deref(), step, tail)
                .await
            {
                Ok(()) => return,
                Err(reply) => reply,
            },
            ReadRequest::Log {
                job,
                step,
                tail,
                client,
                ..
            } => match self.log(&viewer, &job, client.as_deref(), step, tail).await {
                Ok(text) => return self.send_log(conn, call.req, &text),
                Err(reply) => reply,
            },
            ReadRequest::Stats(f) => self.stats(&viewer, f).await,
            ReadRequest::Runners => ReadReply::Runners {
                runners: self.runner_infos(&viewer),
            },
            ReadRequest::Subscribe { topics } => self.subscribe(conn, topics),
            ReadRequest::Unsubscribe { sub } => {
                match self.conns.get_mut(&conn).and_then(|c| c.subs.remove(&sub)) {
                    Some(_) => ReadReply::Unsubscribed { sub },
                    None => not_found_text(format!("subscription {sub}")),
                }
            }
            ReadRequest::Unfollow { req } => match self.unfollow(conn, req) {
                true => ReadReply::Unsubscribed { sub: req },
                false => not_found_text(format!("followed log {req}")),
            },
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

    fn subscribe(&mut self, conn: ConnId, topics: Vec<lr_proto::Topic>) -> ReadReply {
        let held = self.held_streams(conn);
        if held >= MAX_SUBSCRIPTIONS {
            return ReadReply::Failed {
                message: format!("at most {MAX_SUBSCRIPTIONS} subscriptions per connection"),
            };
        }
        let sub = self.sub_id();
        if let Some(c) = self.conns.get_mut(&conn) {
            c.subs.insert(sub, topics);
        }
        ReadReply::Subscribed { sub }
    }

    pub(crate) fn send_log(&mut self, conn: ConnId, req: u64, text: &str) {
        let parts = split_text(text, CHUNK_TEXT_LIMIT);
        let last = parts.len().saturating_sub(1);
        let mut frames: Vec<_> = parts
            .into_iter()
            .enumerate()
            .filter_map(|(i, part)| Frame::json(&log_chunk(req, part.to_string(), i == last)).ok())
            .collect();
        if frames.is_empty() {
            frames.extend(Frame::json(&log_chunk(req, String::new(), true)).ok());
        }
        self.send_ordered(conn, frames);
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
                (Some(r), true) => crate::api::live_steps(
                    r.steps.iter().map(crate::api::step_view).collect(),
                    summary.state,
                    job.started_at,
                ),
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
                wait_code: a
                    .sees_fields()
                    .then(|| self.wait_codes.get(&job.key).map(|c| c.to_string()))
                    .flatten(),
            });
        }
        let (queue, next) = page(queue, &f.cursor, limit as u64);
        let mut recent = Vec::new();
        for rec in self.store.jobs(&scan()).await.unwrap_or_default() {
            if !rec.job.state.is_final() || recent.len() >= limit {
                continue;
            }
            let a = self.access_to(viewer, &rec.job.key.client);
            if let Some(s) = mask_summary(record_summary(&rec), a) {
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

    pub(crate) async fn run_of(
        &self,
        viewer: &Viewer,
        job: &JobId,
        client: Option<&str>,
    ) -> Result<(JobRecord, Access), ReadReply> {
        let named = client.unwrap_or(&viewer.key);
        let key = JobKey {
            client: named.to_string(),
            job: job.clone(),
        };
        if let Ok(Some(rec)) = self.store.job(&key).await {
            return match self.access_to(viewer, named) {
                Access::Hidden => Err(not_found(job)),
                a => Ok((rec, a)),
            };
        }
        if client.is_some() {
            return Err(not_found(job));
        }
        let rows = self.store.jobs(&scan()).await.unwrap_or_default();
        let mut found: Vec<(JobRecord, Access)> = rows
            .into_iter()
            .filter(|rec| &rec.job.key.job == job)
            .filter_map(|rec| {
                let a = self.access_to(viewer, &rec.job.key.client);
                (a != Access::Hidden).then_some((rec, a))
            })
            .collect();
        match found.len() {
            0 => Err(not_found(job)),
            1 => Ok(found.remove(0)),
            _ => {
                let clients: Vec<&str> = found
                    .iter()
                    .map(|(r, _)| r.job.key.client.as_str())
                    .collect();
                Err(not_found_text(format!(
                    "job {job}: this JobId is used by several clients ({}); name the client",
                    clients.join(", ")
                )))
            }
        }
    }

    async fn explain(&self, viewer: &Viewer, job: JobId, client: Option<&str>) -> ReadReply {
        let (rec, a) = match self.run_of(viewer, &job, client).await {
            Ok(found) => found,
            Err(reply) => return reply,
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
            None => finished_text(&rec.job, a.sees_fields()),
        };
        ReadReply::Explain { job, text }
    }

    pub(crate) async fn log(
        &self,
        viewer: &Viewer,
        job: &JobId,
        client: Option<&str>,
        step: Option<String>,
        tail: Option<u64>,
    ) -> Result<String, ReadReply> {
        let (rec, a) = self.run_of(viewer, job, client).await?;
        if !a.sees_log() {
            return Err(ReadReply::Denied {
                reason: "полный лог отдаётся только клиенту-владельцу запуска".into(),
            });
        }
        let bytes = self.store.read_log(&rec.job.key).await.unwrap_or_default();
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if let Some(step) = step {
            text = step_section(&text, &step);
        }
        Ok(match tail {
            Some(n) => last_lines(&text, n as usize)
                .iter()
                .map(|l| format!("{l}\n"))
                .collect(),
            None => text,
        })
    }

    async fn tail(&self, key: &JobKey, n: usize) -> Vec<String> {
        let bytes = self
            .store
            .read_log_tail(key, TAIL_BYTES)
            .await
            .unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        last_lines(&text, n).iter().map(|l| l.to_string()).collect()
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
}

fn scan() -> JobFilter {
    JobFilter {
        limit: Some(SCAN_LIMIT),
        ..JobFilter::default()
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

fn finished_text(row: &JobRow, full: bool) -> String {
    let mut text = format!("завершена: {}", state_name(row.state));
    if !full {
        return text;
    }
    if let Some(reason) = &row.cancel_reason {
        text.push_str(&format!(" ({reason})"));
    }
    if let Some(peaks) = peaks_text(row) {
        text.push_str(&format!("; {peaks}"));
    }
    text
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
