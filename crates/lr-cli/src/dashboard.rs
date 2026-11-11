//! `dashboard` (spec 9): the loopback page over a [`DataSource`] that reads the hub through its
//! CLI channel and builds the [`DashboardSnapshot`] model from the brokers' answers. The URL
//! carries the launch token, so it is printed to the terminal only and never logged.

use crate::commands::jobs::hub;
use crate::commands::log_cmd::cli_redactor;
use crate::commands::view::{known_brokers, read};
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use async_trait::async_trait;
use lr_abstractions::{CancelRequest, DataError, DataSource, EventStream};
use lr_core::{JobId, JobState, Timestamp};
use lr_dashboard::{Dashboard, DashboardOptions, LOG_TAIL_BYTES};
use lr_obs::Redactor;
use lr_proto::{
    BrokerRunners, CancelTarget, CliReply, CliRequest, DashboardEvent, DashboardJob, DashboardList,
    DashboardSnapshot, HistoryFilter, HubOrder, JobDetail, JobSummary, QueueEvent, ReadReply,
    ReadRequest, StatsFilter, StatsView, StatusFilter, StepView,
};
use std::sync::Arc;
use std::time::Duration;

/// How often the page is told to refresh when the hub does not relay its brokers' events (a
/// hub older than [`CliRequest::Events`]).
pub const REFRESH: Duration = Duration::from_secs(2);

/// How long the dashboard waits for the hub to open its event stream.
pub const EVENTS_WAIT: Duration = Duration::from_secs(5);

/// The dashboard's data from the hub of this OS user.
pub struct HubSource {
    layout: Layout,
    last: std::sync::Mutex<Option<DashboardSnapshot>>,
}

impl HubSource {
    /// Data from the hub of `layout`.
    pub fn new(layout: Layout) -> HubSource {
        HubSource {
            layout,
            last: std::sync::Mutex::new(None),
        }
    }
}

fn unavailable(e: impl ToString) -> DataError {
    DataError::Unavailable(e.to_string())
}

/// A card from a list summary.
pub fn detail_of(summary: JobSummary, steps: Vec<StepView>, wait: Option<String>) -> JobDetail {
    JobDetail {
        summary,
        note: None,
        branch: None,
        cwd: None,
        commit: None,
        os_user: None,
        source: None,
        github_run_url: None,
        steps,
        secrets: Vec::new(),
        cancelled_by: None,
        wait_reason: wait,
    }
}

fn job(broker: &str, list: DashboardList, detail: JobDetail) -> DashboardJob {
    DashboardJob {
        broker: Some(broker.to_string()),
        list,
        position: None,
        detail,
        last_lines: Vec::new(),
        stale: false,
    }
}

/// The last [`LOG_TAIL_BYTES`] of a log copy as whole lines masked by `redactor`: a line cut by
/// the start of the tail is left out, so a secret it carried cannot pass half-masked.
pub fn masked_tail(bytes: &[u8], redactor: &Redactor) -> Vec<u8> {
    let start = bytes.len().saturating_sub(LOG_TAIL_BYTES);
    let mut tail = &bytes[start..];
    if start > 0 && bytes[start - 1] != b'\n' {
        let skip = tail
            .iter()
            .position(|b| *b == b'\n')
            .map_or(tail.len(), |i| i + 1);
        tail = &tail[skip..];
    }
    redactor
        .redact_text(&String::from_utf8_lossy(tail))
        .into_bytes()
}

fn merge_stats(into: &mut StatsView, more: StatsView) {
    into.profiles.extend(more.profiles);
    into.clients.extend(more.clients);
}

fn now() -> Timestamp {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    Timestamp(ms)
}

struct Ticks;

#[async_trait]
impl EventStream<DashboardEvent> for Ticks {
    async fn next(&mut self) -> Option<DashboardEvent> {
        tokio::time::sleep(REFRESH).await;
        Some(DashboardEvent {
            broker: "hub".into(),
            event: QueueEvent::Unknown,
        })
    }
}

struct HubEvents(IpcClient);

#[async_trait]
impl EventStream<DashboardEvent> for HubEvents {
    async fn next(&mut self) -> Option<DashboardEvent> {
        loop {
            match self.0.recv::<CliReply>().await.ok()? {
                CliReply::Event(e) => return Some(e),
                CliReply::Error(_) => return None,
                _ => {}
            }
        }
    }
}

/// The live events of the hub of `layout` (spec 9.3): its brokers' queue and runner events,
/// each masked by its broker for this hub's key. A hub that does not open the stream within
/// [`EVENTS_WAIT`] (one older than [`CliRequest::Events`] ignores it) gives refresh ticks
/// every [`REFRESH`] instead.
pub async fn hub_events(
    layout: &Layout,
) -> Result<Box<dyn EventStream<DashboardEvent>>, DataError> {
    let client = hub(layout).await.map_err(unavailable)?;
    let opened = tokio::time::timeout(EVENTS_WAIT, client.call::<_, CliReply>(&CliRequest::Events));
    match opened.await {
        Ok(Ok(CliReply::Watching)) => Ok(Box::new(HubEvents(client))),
        Ok(Err(e)) => Err(unavailable(e)),
        _ => {
            client.close().await;
            Ok(Box::new(Ticks))
        }
    }
}

/// The model of the dashboard and of `status --html` from every reachable broker: running jobs
/// and the queue (filtered by `filter`), finished runs when `history`, runners and statistics.
/// The last lines of running jobs pass `redactor`.
pub async fn build_snapshot(
    client: &IpcClient,
    filter: &StatusFilter,
    history: bool,
    redactor: &Redactor,
) -> Result<DashboardSnapshot, String> {
    let brokers = known_brokers(client).await?;
    let mut snap = DashboardSnapshot {
        taken_at: now(),
        brokers: brokers.clone(),
        jobs: Vec::new(),
        runners: Vec::new(),
        stats: StatsView {
            profiles: Vec::new(),
            clients: Vec::new(),
        },
    };
    for b in brokers.iter().filter(|b| b.available) {
        let name = b.name.as_str();
        let status = read(client, name, ReadRequest::Status(filter.clone())).await;
        let Ok(ReadReply::Status(status)) = status else {
            continue;
        };
        for r in status.running {
            let mut j = job(
                name,
                DashboardList::Running,
                detail_of(r.summary, r.steps, None),
            );
            j.last_lines = r
                .last_lines
                .iter()
                .map(|l| redactor.redact_text(l))
                .collect();
            snap.jobs.push(j);
        }
        for q in status.queue {
            let mut j = job(
                name,
                DashboardList::Queued,
                detail_of(q.summary, Vec::new(), q.wait_reason),
            );
            j.position = Some(q.position);
            snap.jobs.push(j);
        }
        let finished = HistoryFilter {
            project: filter.project.clone(),
            source: filter.source,
            label: filter.label.clone(),
            limit: Some(100),
            ..HistoryFilter::default()
        };
        let answer = match history {
            true => Some(read(client, name, ReadRequest::History(finished)).await),
            false => None,
        };
        if let Some(Ok(ReadReply::History { jobs, .. })) = answer {
            for s in jobs.into_iter().filter(|s| s.state.is_final()) {
                snap.jobs.push(job(
                    name,
                    DashboardList::History,
                    detail_of(s, Vec::new(), None),
                ));
            }
        }
        if let Ok(ReadReply::Runners { runners }) = read(client, name, ReadRequest::Runners).await {
            snap.runners.push(BrokerRunners {
                broker: name.to_string(),
                pool: status.pool,
                runners,
                stale: false,
            });
        }
        if let Ok(ReadReply::Stats(s)) =
            read(client, name, ReadRequest::Stats(StatsFilter::default())).await
        {
            merge_stats(&mut snap.stats, s);
        }
    }
    add_hub_orders(&mut snap, hub_orders(client).await, filter);
    Ok(snap)
}

/// The hub's active orders; empty when the hub does not answer within [`ORDERS_WAIT`].
pub async fn hub_orders(client: &IpcClient) -> Vec<HubOrder> {
    let call = client.call::<_, CliReply>(&CliRequest::Orders);
    match tokio::time::timeout(ORDERS_WAIT, call).await {
        Ok(Ok(CliReply::Orders { orders, .. })) => orders,
        _ => Vec::new(),
    }
}

/// How long the dashboard waits for the hub's list of its orders.
pub const ORDERS_WAIT: Duration = Duration::from_secs(5);

fn matches(filter: &StatusFilter, s: &JobSummary) -> bool {
    filter
        .project
        .as_ref()
        .is_none_or(|p| s.project.as_ref() == Some(p))
        && filter
            .label
            .as_ref()
            .is_none_or(|l| s.label.as_ref() == Some(l))
        && filter.source.is_none_or(|w| s.source == Some(w))
}

/// Adds the orders of the hub the brokers did not report: those waiting in its outbox, and
/// those of an unreachable broker, marked stale (spec 9.3).
pub fn add_hub_orders(snap: &mut DashboardSnapshot, orders: Vec<HubOrder>, filter: &StatusFilter) {
    for o in orders.into_iter().filter(|o| matches(filter, &o.summary)) {
        if snap
            .jobs
            .iter()
            .any(|j| j.detail.summary.job == o.summary.job)
        {
            continue;
        }
        let reachable = |b: &str| snap.brokers.iter().any(|s| s.name == b && s.available);
        let (list, stale) = match (&o.broker, o.outbox) {
            (_, true) => (DashboardList::Outbox, false),
            (Some(b), false) if !reachable(b) => match o.summary.state {
                JobState::Running => (DashboardList::Running, true),
                _ => (DashboardList::Queued, true),
            },
            _ => continue,
        };
        snap.jobs.push(DashboardJob {
            broker: o.broker,
            list,
            position: None,
            detail: detail_of(o.summary, Vec::new(), None),
            last_lines: Vec::new(),
            stale,
        });
    }
}

/// Keeps the last known rows of every broker `snap` found unreachable, marked stale: its jobs
/// (in place of the hub's thinner rows of the same jobs) and its runners (spec 9.3).
pub fn keep_last_known(snap: &mut DashboardSnapshot, last: Option<&DashboardSnapshot>) {
    let Some(last) = last else {
        return;
    };
    let down: Vec<String> = snap
        .brokers
        .iter()
        .filter(|b| !b.available)
        .map(|b| b.name.clone())
        .collect();
    let of_down = |b: &Option<String>| b.as_ref().is_some_and(|b| down.contains(b));
    for old in last
        .jobs
        .iter()
        .filter(|j| of_down(&j.broker) && j.list != DashboardList::Outbox)
    {
        let kept = DashboardJob {
            stale: true,
            ..old.clone()
        };
        let id = &old.detail.summary.job;
        match snap.jobs.iter_mut().find(|j| &j.detail.summary.job == id) {
            Some(j) if j.stale => *j = kept,
            Some(_) => {}
            None => snap.jobs.push(kept),
        }
    }
    for old in last.runners.iter().filter(|r| down.contains(&r.broker)) {
        if !snap.runners.iter().any(|r| r.broker == old.broker) {
            snap.runners.push(BrokerRunners {
                stale: true,
                ..old.clone()
            });
        }
    }
}

/// The log copy of `job` for the page: from the directory of the run a reachable broker
/// knows, else the copy the hub keeps for its own active order; masked by `redactor`.
pub async fn log_copy(
    client: &IpcClient,
    job: &JobId,
    redactor: &Redactor,
) -> Result<Vec<u8>, DataError> {
    let safe = !job.0.is_empty()
        && job.0.len() <= 64
        && job.0.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !safe {
        return Err(DataError::NotFound);
    }
    let brokers = known_brokers(client).await.map_err(unavailable)?;
    for b in brokers.iter().filter(|b| b.available) {
        let req = ReadRequest::HistoryShow {
            job: job.clone(),
            client: None,
        };
        if let Ok(ReadReply::Job(d)) = read(client, &b.name, req).await {
            let Some(cwd) = d.cwd else {
                return Err(DataError::NotFound);
            };
            let dir = std::path::Path::new(&cwd);
            if !clean_absolute(dir) {
                return Err(DataError::NotFound);
            }
            let path = dir.join("temp").join("runner").join(format!("{job}.log"));
            let bytes = std::fs::read(&path).map_err(|_| DataError::NotFound)?;
            return Ok(masked_tail(&bytes, redactor));
        }
    }
    let order = hub_orders(client)
        .await
        .into_iter()
        .find(|o| &o.summary.job == job);
    let Some(path) = order.and_then(|o| o.log) else {
        return Err(DataError::NotFound);
    };
    let path = std::path::Path::new(&path);
    let named = path.file_name() == Some(std::ffi::OsStr::new(&format!("{job}.log")));
    if !named || !clean_absolute(path) {
        return Err(DataError::NotFound);
    }
    let bytes = std::fs::read(path).map_err(|_| DataError::NotFound)?;
    Ok(masked_tail(&bytes, redactor))
}

fn clean_absolute(path: &std::path::Path) -> bool {
    path.is_absolute()
        && path.components().all(|c| {
            !matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
}

#[async_trait]
impl DataSource for HubSource {
    type Snapshot = DashboardSnapshot;
    type Event = DashboardEvent;

    async fn snapshot(&self) -> Result<DashboardSnapshot, DataError> {
        let client = hub(&self.layout).await.map_err(unavailable)?;
        let redactor = cli_redactor(&self.layout);
        let snap = build_snapshot(&client, &StatusFilter::default(), true, &redactor).await;
        client.close().await;
        let mut snap = snap.map_err(unavailable)?;
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        keep_last_known(&mut snap, last.as_ref());
        *last = Some(snap.clone());
        Ok(snap)
    }

    async fn subscribe(&self) -> Result<Box<dyn EventStream<DashboardEvent>>, DataError> {
        hub_events(&self.layout).await
    }

    async fn log(&self, job: &JobId) -> Result<Vec<u8>, DataError> {
        let client = hub(&self.layout).await.map_err(unavailable)?;
        let copy = log_copy(&client, job, &cli_redactor(&self.layout)).await;
        client.close().await;
        copy
    }

    async fn cancel(&self, job: &JobId, request: CancelRequest) -> Result<(), DataError> {
        let cwd = std::env::current_dir().map_err(unavailable)?;
        let actor =
            crate::facts::collect_session(&request.actor, None, &cwd).map_err(unavailable)?;
        let client = hub(&self.layout).await.map_err(unavailable)?;
        let req = CliRequest::Cancel(lr_proto::CancelRequest {
            actor,
            target: CancelTarget::Job(job.clone()),
            force: true,
            reason: request.reason,
        });
        match client
            .call::<_, CliReply>(&req)
            .await
            .map_err(unavailable)?
        {
            CliReply::Cancelled { .. } => Ok(()),
            CliReply::Error(e) => Err(DataError::Refused(e.message)),
            other => Err(DataError::Refused(format!("{other:?}"))),
        }
    }
}

/// `dashboard`: serves until Ctrl+C. The URL goes to stdout only.
pub async fn run(layout: &Layout) -> Result<i32, String> {
    let user = lr_net::current_os_user().map_err(|e| e.to_string())?;
    let source = Arc::new(HubSource::new(layout.clone()));
    let dashboard = Dashboard::new(source, DashboardOptions { user }).map_err(|e| e.to_string())?;
    let running = dashboard.serve().await.map_err(|e| e.to_string())?;
    println!("{}", running.url());
    eprintln!("dashboard on loopback; Ctrl+C stops it");
    let _ = tokio::signal::ctrl_c().await;
    running.shutdown().await;
    Ok(0)
}
