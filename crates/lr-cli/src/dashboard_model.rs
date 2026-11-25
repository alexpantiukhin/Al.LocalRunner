//! The [`DashboardSnapshot`] model of the dashboard and of `status --html` (spec 9.2), built
//! from the answers of every reachable broker and the hub's own orders.

use crate::commands::view::{known_brokers, read};
use crate::ipc_client::IpcClient;
use lr_abstractions::HistoryWindow;
use lr_core::{JobState, Timestamp};
use lr_obs::Redactor;
use lr_proto::{
    effective_limit, BrokerRunners, CliReply, CliRequest, DashboardJob, DashboardList,
    DashboardSnapshot, GridCount, HistoryFilter, HistoryPage, HubOrder, JobDetail, JobSummary,
    ReadReply, ReadRequest, StatsFilter, StatsView, StatusFilter, StepView, HISTORY_LIMIT_MAX,
};
use std::time::Duration;

/// How long the dashboard waits for the hub's list of its orders.
pub const ORDERS_WAIT: Duration = Duration::from_secs(5);

/// How many finished runs of a broker the model carries by default; a page may ask for more, up
/// to what the broker serves at most.
pub const HISTORY_LIMIT: u64 = 100;

/// A card from a list summary: the order fields the summary carries (source, directory, note,
/// branch, run link) are copied, so a summary its broker masked leaves them empty.
pub fn detail_of(summary: JobSummary, steps: Vec<StepView>, wait: Option<String>) -> JobDetail {
    JobDetail {
        note: summary.note.clone(),
        branch: summary.branch.clone(),
        cwd: summary.cwd.clone(),
        os_user: None,
        source: summary.source,
        github_run_url: summary.github_run_url.clone(),
        summary,
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
        wait_code: None,
        priority: None,
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

/// The finished runs of `broker` as cards: full records with steps and canceller from a broker
/// that answers [`ReadRequest::HistoryDetails`], else cards from the history list. Each card is
/// masked by its broker for this hub's key.
pub async fn finished_runs(
    client: &IpcClient,
    broker: &str,
    filter: HistoryFilter,
) -> Vec<JobDetail> {
    finished_page(client, broker, filter).await.0
}

/// As [`finished_runs`], with how many runs the broker holds in all for the filter; `None` from
/// a broker that does not report it, which also ignores an offset.
pub async fn finished_page(
    client: &IpcClient,
    broker: &str,
    filter: HistoryFilter,
) -> (Vec<JobDetail>, Option<u64>) {
    let details = read(client, broker, ReadRequest::HistoryDetails(filter.clone())).await;
    let (cards, total) = match details {
        Ok(ReadReply::Details { jobs, total, .. }) => (jobs, total),
        _ => match read(client, broker, ReadRequest::History(filter)).await {
            Ok(ReadReply::History { jobs, total, .. }) => (
                jobs.into_iter()
                    .map(|s| detail_of(s, Vec::new(), None))
                    .collect(),
                total,
            ),
            _ => (Vec::new(), None),
        },
    };
    let cards = cards
        .into_iter()
        .filter(|d| d.summary.state.is_final())
        .collect();
    (cards, total)
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
    let window = history.then(HistoryWindow::default);
    build_snapshot_page(client, filter, window, redactor).await
}

/// As [`build_snapshot`], with the part of the history that `window` names (none without
/// history). Every broker is asked for the same offset and size; the totals add up. A broker
/// that does not report its total cannot page: it gives its first page only, and the snapshot
/// then carries no paging.
pub async fn build_snapshot_page(
    client: &IpcClient,
    filter: &StatusFilter,
    window: Option<HistoryWindow>,
    redactor: &Redactor,
) -> Result<DashboardSnapshot, String> {
    let brokers = known_brokers(client).await?;
    let limit = effective_limit(
        window.and_then(|w| w.limit),
        HISTORY_LIMIT,
        HISTORY_LIMIT_MAX,
    );
    let offset = window.map_or(0, |w| w.offset);
    let (mut queue_shown, mut queue_all, mut queue_known) = (0u64, 0u64, true);
    let (mut history_all, mut history_known, mut history_asked) = (0u64, true, false);
    let mut snap = DashboardSnapshot {
        taken_at: now(),
        brokers: brokers.clone(),
        jobs: Vec::new(),
        runners: Vec::new(),
        stats: StatsView {
            profiles: Vec::new(),
            clients: Vec::new(),
        },
        history_page: None,
        queue_count: None,
    };
    for b in brokers.iter().filter(|b| b.available) {
        let name = b.name.as_str();
        let status = read(client, name, ReadRequest::Status(filter.clone())).await;
        let Ok(ReadReply::Status(status)) = status else {
            continue;
        };
        let status = *status;
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
        queue_shown += status.queue.len() as u64;
        match status.queue_total {
            Some(t) => queue_all += u64::from(t),
            None => queue_known = false,
        }
        for q in status.queue {
            let mut j = job(
                name,
                DashboardList::Queued,
                detail_of(q.summary, Vec::new(), q.wait_reason),
            );
            j.position = Some(q.position);
            j.wait_code = q.wait_code;
            j.priority = q.priority;
            snap.jobs.push(j);
        }
        if window.is_some() {
            let finished = HistoryFilter {
                project: filter.project.clone(),
                source: filter.source,
                label: filter.label.clone(),
                limit: Some(limit),
                offset: (offset > 0).then_some(offset),
                ..HistoryFilter::default()
            };
            let (cards, total) = finished_page(client, name, finished).await;
            history_asked = true;
            match total {
                Some(t) => history_all += t,
                None => history_known = false,
            }
            if offset == 0 || total.is_some() {
                for d in cards {
                    snap.jobs.push(job(name, DashboardList::History, d));
                }
            }
        }
        if let Ok(ReadReply::Runners { runners, .. }) =
            read(client, name, ReadRequest::Runners).await
        {
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
    if queue_known && brokers.iter().any(|b| b.available) {
        snap.queue_count = Some(GridCount {
            shown: queue_shown,
            total: queue_all,
        });
    }
    if history_asked && history_known {
        snap.history_page = Some(HistoryPage {
            offset,
            limit,
            total: history_all,
        });
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
            wait_code: None,
            priority: None,
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
