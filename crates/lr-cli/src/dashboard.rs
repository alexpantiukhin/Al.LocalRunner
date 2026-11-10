//! `dashboard` (spec 9): the loopback page over a [`DataSource`] that reads the hub through its
//! CLI channel and builds the [`DashboardSnapshot`] model from the brokers' answers. The URL
//! carries the launch token, so it is printed to the terminal only and never logged.

use crate::commands::jobs::hub;
use crate::commands::view::{known_brokers, read};
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use async_trait::async_trait;
use lr_abstractions::{CancelRequest, DataError, DataSource, EventStream};
use lr_core::{JobId, Timestamp};
use lr_dashboard::{Dashboard, DashboardOptions, LOG_TAIL_BYTES};
use lr_proto::{
    BrokerRunners, CancelTarget, CliReply, CliRequest, DashboardEvent, DashboardJob, DashboardList,
    DashboardSnapshot, HistoryFilter, JobDetail, JobSummary, QueueEvent, ReadReply, ReadRequest,
    StatsFilter, StatsView, StatusFilter, StepView,
};
use std::sync::Arc;
use std::time::Duration;

/// How often the page is told to refresh: the hub relays no broker events to the CLI yet.
pub const REFRESH: Duration = Duration::from_secs(2);

/// The dashboard's data from the hub of this OS user.
pub struct HubSource {
    layout: Layout,
}

impl HubSource {
    /// Data from the hub of `layout`.
    pub fn new(layout: Layout) -> HubSource {
        HubSource { layout }
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

/// The model of the dashboard and of `status --html` from every reachable broker: running jobs
/// and the queue (filtered by `filter`), finished runs when `history`, runners and statistics.
pub async fn build_snapshot(
    client: &IpcClient,
    filter: &StatusFilter,
    history: bool,
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
            j.last_lines = r.last_lines;
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
    Ok(snap)
}

#[async_trait]
impl DataSource for HubSource {
    type Snapshot = DashboardSnapshot;
    type Event = DashboardEvent;

    async fn snapshot(&self) -> Result<DashboardSnapshot, DataError> {
        let client = hub(&self.layout).await.map_err(unavailable)?;
        let snap = build_snapshot(&client, &StatusFilter::default(), true).await;
        client.close().await;
        snap.map_err(unavailable)
    }

    async fn subscribe(&self) -> Result<Box<dyn EventStream<DashboardEvent>>, DataError> {
        Ok(Box::new(Ticks))
    }

    async fn log(&self, job: &JobId) -> Result<Vec<u8>, DataError> {
        let safe = !job.0.is_empty()
            && job.0.len() <= 64
            && job.0.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !safe {
            return Err(DataError::NotFound);
        }
        let client = hub(&self.layout).await.map_err(unavailable)?;
        let brokers = known_brokers(&client).await.map_err(unavailable)?;
        for b in brokers.iter().filter(|b| b.available) {
            let req = ReadRequest::HistoryShow {
                job: job.clone(),
                client: None,
            };
            if let Ok(ReadReply::Job(d)) = read(&client, &b.name, req).await {
                let Some(cwd) = d.cwd else {
                    return Err(DataError::NotFound);
                };
                let dir = std::path::Path::new(&cwd);
                let clean = dir.is_absolute()
                    && dir.components().all(|c| {
                        !matches!(
                            c,
                            std::path::Component::ParentDir | std::path::Component::CurDir
                        )
                    });
                if !clean {
                    return Err(DataError::NotFound);
                }
                let path = dir.join("temp").join("runner").join(format!("{job}.log"));
                let bytes = std::fs::read(&path).map_err(|_| DataError::NotFound)?;
                let start = bytes.len().saturating_sub(LOG_TAIL_BYTES);
                return Ok(bytes[start..].to_vec());
            }
        }
        Err(DataError::NotFound)
    }

    async fn cancel(&self, job: &JobId, request: CancelRequest) -> Result<(), DataError> {
        let cwd = std::env::current_dir().map_err(unavailable)?;
        let actor = crate::facts::collect_session("dashboard", None, &cwd).map_err(unavailable)?;
        let client = hub(&self.layout).await.map_err(unavailable)?;
        let req = CliRequest::Cancel(lr_proto::CancelRequest {
            actor,
            target: CancelTarget::Job(job.clone()),
            force: true,
            reason: Some(match request.reason {
                Some(r) => format!("{}: {r}", request.actor),
                None => request.actor,
            }),
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
