mod common;

use common::TempDir;
use lr_abstractions::fakes::{channel_pair, ManualClock};
use lr_abstractions::{DataError, Frame, Peer, PeerAccess, Transport};
use lr_cli::commands::snapshot_cmd::{log_targets, readable_logs, SNAPSHOT_LOG_LINES};
use lr_cli::dashboard::{build_snapshot, keep_last_known, log_copy};
use lr_cli::ipc_client::IpcClient;
use lr_core::{Endpoint, JobId, JobState, Timestamp, Zone};
use lr_obs::Redactor;
use lr_proto::{
    BrokerStatus, CliReply, CliRequest, DashboardList, DashboardSnapshot, HubOrder, IpcChannel,
    IpcOpenReply, JobSummary, PoolLoad, ProfileStatsView, QueueItem, ReadReply, ReadRequest,
    ReasonCode, RunningView, SpreadView, StatsView, StatusFilter, StatusView, PROTOCOL_VERSION,
};
use std::sync::{Arc, Mutex};

type Answer = Box<dyn Fn(&CliRequest) -> CliReply + Send>;

async fn fake_hub(answer: Answer) -> (IpcClient, Arc<Mutex<Vec<CliRequest>>>) {
    let peer = |user: &str| Peer {
        endpoint: Endpoint::Ipc {
            os_user: user.into(),
        },
        key: None,
        access: PeerAccess::Local,
    };
    let clock = Arc::new(ManualClock::new(Timestamp(0)));
    let (cli, hub) = channel_pair(clock, peer("alex"), peer("alex"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        let _hello = hub.recv().await.expect("hello");
        let ready = IpcOpenReply::Ready {
            version: PROTOCOL_VERSION,
        };
        hub.send(Frame::json(&ready).unwrap()).await.unwrap();
        while let Ok(frame) = hub.recv().await {
            let req: CliRequest = lr_proto::decode(frame.as_bytes()).unwrap();
            let reply = answer(&req);
            log.lock().unwrap().push(req);
            hub.send(Frame::json(&reply).unwrap()).await.unwrap();
        }
    });
    let client = IpcClient::open(Box::new(cli), IpcChannel::Cli)
        .await
        .expect("the fake hub opens the channel");
    (client, seen)
}

fn summary(job: &str, state: JobState) -> JobSummary {
    JobSummary {
        job: JobId(job.into()),
        client_label: "laptop".into(),
        state,
        platform: Some("linux".into()),
        runner: None,
        queued_at: Timestamp(1_700_000_000_000),
        started_at: None,
        finished_at: None,
        exit_code: None,
        reason: None,
        project: Some("acme/app".into()),
        label: Some("agent".into()),
        profile: Some("unit".into()),
        source: None,
        note: None,
        branch: None,
        cwd: None,
        timeout_s: None,
        github_run_url: None,
        peak_mem_mb: None,
        peak_disk_mb: None,
        labels: Vec::new(),
        cancelled_by: None,
    }
}

fn broker(name: &str, available: bool) -> BrokerStatus {
    BrokerStatus {
        name: name.into(),
        zone: Some(Zone::Lan),
        available,
        outbox: 0,
        since: None,
    }
}

fn status(name: &str) -> StatusView {
    StatusView {
        running: vec![RunningView {
            summary: summary(&format!("run-{name}"), JobState::Running),
            steps: Vec::new(),
            last_lines: vec![format!("{name} api_token=hunter2hunter2")],
            last_output_ago_ms: None,
        }],
        queue: vec![QueueItem {
            summary: summary(&format!("wait-{name}"), JobState::Queued),
            position: 1,
            wait_reason: Some(format!("runner of {name} busy")),
            wait_code: None,
        }],
        recent: Vec::new(),
        pool: PoolLoad {
            used_mem_mb: 512,
            total_mem_mb: 4096,
            used_cpu: 1,
            total_cpu: 4,
        },
        next: None,
        queue_total: None,
    }
}

fn stats(profile: &str) -> StatsView {
    let spread = SpreadView {
        median: None,
        p95: None,
    };
    StatsView {
        profiles: vec![ProfileStatsView {
            mem: None,
            profile: profile.into(),
            runs: 1,
            passed: 1,
            failed: 0,
            timeouts: 0,
            cancelled: 0,
            lost: 0,
            wait_ms: spread,
            duration_ms: spread,
            steps: Vec::new(),
        }],
        clients: Vec::new(),
    }
}

fn hub_order(job: &str, broker: Option<&str>, outbox: bool, log: Option<String>) -> HubOrder {
    HubOrder {
        broker: broker.map(str::to_string),
        outbox,
        summary: summary(job, JobState::Queued),
        log,
    }
}

fn hub(
    up: &'static [&'static str],
    down: &'static [&'static str],
    orders: Vec<HubOrder>,
) -> Answer {
    Box::new(move |req| match req {
        CliRequest::Brokers => CliReply::Brokers {
            brokers: up
                .iter()
                .map(|b| broker(b, true))
                .chain(down.iter().map(|b| broker(b, false)))
                .collect(),
        },
        CliRequest::Orders => CliReply::Orders {
            orders: orders.clone(),
            truncated: false,
        },
        CliRequest::Read(r) => {
            let name = r.broker.clone().unwrap();
            if let ReadRequest::Log { job, .. } = &r.request {
                if job.0 == format!("run-{name}") {
                    return CliReply::LogChunk {
                        broker: name.clone(),
                        text: format!("{name} step\napi_token=hunter2hunter2\n"),
                        end: true,
                    };
                }
                if job.0 == format!("done-{name}") {
                    return CliReply::Read {
                        broker: name,
                        reply: ReadReply::Denied {
                            reason: "not yours".into(),
                        },
                    };
                }
            }
            let reply = match &r.request {
                ReadRequest::Status(_) => ReadReply::Status(status(&name)),
                ReadRequest::History(_) => ReadReply::History {
                    jobs: vec![
                        JobSummary {
                            reason: Some(ReasonCode::SenderGone),
                            ..summary(&format!("done-{name}"), JobState::Cancelled)
                        },
                        summary(&format!("still-{name}"), JobState::Running),
                    ],
                    next: None,
                    total: None,
                },
                ReadRequest::Runners => ReadReply::Runners {
                    runners: vec![],
                    pools: vec![],
                },
                ReadRequest::Stats(_) => ReadReply::Stats(stats(&format!("unit-{name}"))),
                _ => ReadReply::NotFound { what: name.clone() },
            };
            CliReply::Read {
                broker: name,
                reply,
            }
        }
        other => panic!("unexpected request {other:?}"),
    })
}

fn rows(snap: &DashboardSnapshot) -> Vec<(String, Option<String>, DashboardList, bool)> {
    let mut rows: Vec<_> = snap
        .jobs
        .iter()
        .map(|j| {
            (
                j.detail.summary.job.0.clone(),
                j.broker.clone(),
                j.list,
                j.stale,
            )
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

fn row(
    job: &str,
    broker: Option<&str>,
    list: DashboardList,
    stale: bool,
) -> (String, Option<String>, DashboardList, bool) {
    (job.into(), broker.map(str::to_string), list, stale)
}

#[tokio::test]
async fn snapshot_collects_queue_runners_and_history_from_every_broker() {
    let orders = vec![hub_order("out-1", None, true, None)];
    let (client, seen) = fake_hub(hub(&["home", "office"], &[], orders)).await;
    let redactor = Redactor::new(&[]);
    let snap = build_snapshot(&client, &StatusFilter::default(), true, &redactor)
        .await
        .unwrap();
    use DashboardList::*;
    assert_eq!(
        rows(&snap),
        [
            row("done-home", Some("home"), History, false),
            row("done-office", Some("office"), History, false),
            row("out-1", None, Outbox, false),
            row("run-home", Some("home"), Running, false),
            row("run-office", Some("office"), Running, false),
            row("wait-home", Some("home"), Queued, false),
            row("wait-office", Some("office"), Queued, false),
        ],
        "a history row that is not final is left out"
    );
    let waiting = snap
        .jobs
        .iter()
        .find(|j| j.detail.summary.job.0 == "wait-office")
        .unwrap();
    assert_eq!(waiting.position, Some(1));
    assert_eq!(
        waiting.detail.wait_reason.as_deref(),
        Some("runner of office busy")
    );
    let done = snap
        .jobs
        .iter()
        .find(|j| j.detail.summary.job.0 == "done-home")
        .unwrap();
    assert_eq!(done.detail.summary.reason, Some(ReasonCode::SenderGone));
    let json = serde_json::to_value(&snap).unwrap();
    let text = json.to_string();
    assert!(text.contains("\"sender_gone\""), "{text}");
    assert!(text.contains("\"runner of office busy\""), "{text}");
    let running = snap
        .jobs
        .iter()
        .find(|j| j.detail.summary.job.0 == "run-home")
        .unwrap();
    assert_eq!(running.last_lines, ["home api_token=***"]);
    let pools: Vec<(&str, u64)> = snap
        .runners
        .iter()
        .map(|r| (r.broker.as_str(), r.pool.total_mem_mb))
        .collect();
    assert_eq!(pools, [("home", 4096), ("office", 4096)]);
    let profiles: Vec<&str> = snap
        .stats
        .profiles
        .iter()
        .map(|p| p.profile.as_str())
        .collect();
    assert_eq!(profiles, ["unit-home", "unit-office"]);
    let limits: Vec<Option<u64>> = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|r| match r {
            CliRequest::Read(r) => match &r.request {
                ReadRequest::History(h) => Some(h.limit),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        limits,
        [Some(100), Some(100)],
        "up to 100 finished per broker"
    );
}

fn pages(inner: Answer, total: Option<u64>, queue_total: Option<u64>) -> Answer {
    Box::new(move |req| {
        let CliRequest::Read(r) = req else {
            return inner(req);
        };
        let name = r.broker.clone().unwrap();
        let reply = match &r.request {
            ReadRequest::History(f) => ReadReply::History {
                jobs: vec![summary(
                    &format!("page-{name}-{}", f.offset.unwrap_or(0)),
                    JobState::Passed,
                )],
                next: None,
                total,
            },
            ReadRequest::Status(_) => ReadReply::Status(StatusView {
                queue_total,
                ..status(&name)
            }),
            _ => return inner(req),
        };
        CliReply::Read {
            broker: name,
            reply,
        }
    })
}

fn history_requests(seen: &Arc<Mutex<Vec<CliRequest>>>) -> Vec<(Option<u64>, Option<u64>)> {
    seen.lock()
        .unwrap()
        .iter()
        .filter_map(|r| match r {
            CliRequest::Read(r) => match &r.request {
                ReadRequest::History(h) => Some((h.offset, h.limit)),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_history_page_asks_every_broker_for_the_offset_and_adds_the_totals_up() {
    use lr_abstractions::HistoryWindow;
    use lr_cli::dashboard::build_snapshot_page;
    use lr_proto::{GridCount, HistoryPage};
    let answer = pages(
        hub(&["home", "office"], &[], Vec::new()),
        Some(250),
        Some(80),
    );
    let (client, seen) = fake_hub(answer).await;
    let window = HistoryWindow {
        offset: 100,
        limit: Some(50),
    };
    let snap = build_snapshot_page(
        &client,
        &StatusFilter::default(),
        Some(window),
        &Redactor::new(&[]),
    )
    .await
    .unwrap();
    assert_eq!(
        history_requests(&seen),
        [(Some(100), Some(50)), (Some(100), Some(50))]
    );
    let done: Vec<String> = snap
        .jobs
        .iter()
        .filter(|j| j.list == DashboardList::History)
        .map(|j| j.detail.summary.job.0.clone())
        .collect();
    assert_eq!(done, ["page-home-100", "page-office-100"]);
    assert_eq!(
        snap.history_page,
        Some(HistoryPage {
            offset: 100,
            limit: 50,
            total: 500
        })
    );
    assert_eq!(
        snap.queue_count,
        Some(GridCount {
            shown: 2,
            total: 160
        }),
        "one order shown of 80 on each broker"
    );
    let first = build_snapshot_page(
        &client,
        &StatusFilter::default(),
        Some(HistoryWindow::default()),
        &Redactor::new(&[]),
    )
    .await
    .unwrap();
    assert_eq!(
        history_requests(&seen)[2..],
        [(None, Some(100)), (None, Some(100))],
        "the first page asks for no offset"
    );
    assert_eq!(
        first.history_page.map(|p| (p.offset, p.limit)),
        Some((0, 100))
    );
    let huge = HistoryWindow {
        offset: 0,
        limit: Some(1_000_000),
    };
    build_snapshot_page(
        &client,
        &StatusFilter::default(),
        Some(huge),
        &Redactor::new(&[]),
    )
    .await
    .unwrap();
    assert_eq!(
        history_requests(&seen)[4].1,
        Some(500),
        "the size is bounded"
    );
}

#[tokio::test]
async fn a_broker_that_does_not_report_totals_gives_its_first_page_and_no_paging() {
    use lr_abstractions::HistoryWindow;
    use lr_cli::dashboard::build_snapshot_page;
    let answer = pages(hub(&["old"], &[], Vec::new()), None, None);
    let (client, _) = fake_hub(answer).await;
    let first = build_snapshot_page(
        &client,
        &StatusFilter::default(),
        Some(HistoryWindow::default()),
        &Redactor::new(&[]),
    )
    .await
    .unwrap();
    assert!(first.jobs.iter().any(|j| j.list == DashboardList::History));
    assert_eq!(first.history_page, None);
    assert_eq!(first.queue_count, None);
    let later = build_snapshot_page(
        &client,
        &StatusFilter::default(),
        Some(HistoryWindow {
            offset: 100,
            limit: None,
        }),
        &Redactor::new(&[]),
    )
    .await
    .unwrap();
    assert!(
        later.jobs.iter().all(|j| j.list != DashboardList::History),
        "an older broker ignores the offset, so its first page is not shown again as page two"
    );
    assert_eq!(later.history_page, None);
}

#[tokio::test]
async fn with_logs_a_snapshot_embeds_the_last_500_lines_it_may_read_masked() {
    let (client, seen) = fake_hub(hub(&["home"], &[], Vec::new())).await;
    let redactor = Arc::new(Redactor::new(&[]));
    let snap = build_snapshot(&client, &StatusFilter::default(), true, &redactor)
        .await
        .unwrap();
    let mut targets = log_targets(&snap);
    targets.sort();
    let home = |j: &str| ("home".to_string(), JobId(j.into()));
    assert_eq!(
        targets,
        [home("done-home"), home("run-home")],
        "running and finished jobs only, not the queue"
    );
    let logs = readable_logs(&client, &targets, &redactor).await;
    assert_eq!(
        logs,
        [(
            JobId("run-home".into()),
            "home step\napi_token=***\n".to_string()
        )],
        "a refused log is left out, the rest is masked"
    );
    let tails: Vec<Option<u64>> = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|r| match r {
            CliRequest::Read(r) => match &r.request {
                ReadRequest::Log { tail, follow, .. } => {
                    assert!(!follow);
                    Some(*tail)
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(tails, [Some(SNAPSHOT_LOG_LINES); 2]);
    assert_eq!(SNAPSHOT_LOG_LINES, 500);
}

#[tokio::test]
async fn without_history_a_snapshot_asks_no_broker_for_finished_runs() {
    let (client, seen) = fake_hub(hub(&["home"], &[], Vec::new())).await;
    let snap = build_snapshot(
        &client,
        &StatusFilter::default(),
        false,
        &Redactor::new(&[]),
    )
    .await
    .unwrap();
    assert!(snap.jobs.iter().all(|j| j.list != DashboardList::History));
    assert!(!seen.lock().unwrap().iter().any(|r| matches!(
        r,
        CliRequest::Read(r) if matches!(r.request, ReadRequest::History(_))
    )));
}

#[tokio::test]
async fn stopped_broker_marks_its_data_stale_and_keeps_outbox_orders_and_log_copies() {
    let redactor = Redactor::new(&[]);
    let (before, _) = fake_hub(hub(&["home", "office"], &[], Vec::new())).await;
    let first = build_snapshot(&before, &StatusFilter::default(), true, &redactor)
        .await
        .unwrap();
    let tmp = TempDir::new("dash-stale");
    let copy = tmp.path().join("run-office.log");
    std::fs::write(&copy, "step one\npassword=letmein1\n").unwrap();
    let orders = vec![
        hub_order(
            "run-office",
            Some("office"),
            false,
            Some(copy.display().to_string()),
        ),
        hub_order("late-office", Some("office"), false, None),
        hub_order("out-2", Some("office"), true, None),
    ];
    let (after, _) = fake_hub(hub(&["home"], &["office"], orders)).await;
    let mut snap = build_snapshot(&after, &StatusFilter::default(), true, &redactor)
        .await
        .unwrap();
    keep_last_known(&mut snap, Some(&first));
    use DashboardList::*;
    assert_eq!(
        rows(&snap),
        [
            row("done-home", Some("home"), History, false),
            row("done-office", Some("office"), History, true),
            row("late-office", Some("office"), Queued, true),
            row("out-2", Some("office"), Outbox, false),
            row("run-home", Some("home"), Running, false),
            row("run-office", Some("office"), Running, true),
            row("wait-home", Some("home"), Queued, false),
            row("wait-office", Some("office"), Queued, true),
        ]
    );
    let kept = snap
        .jobs
        .iter()
        .find(|j| j.detail.summary.job.0 == "wait-office")
        .unwrap();
    assert_eq!(
        kept.detail.wait_reason.as_deref(),
        Some("runner of office busy"),
        "the last known card, not a thinner one"
    );
    let runners: Vec<(&str, bool)> = snap
        .runners
        .iter()
        .map(|r| (r.broker.as_str(), r.stale))
        .collect();
    assert_eq!(runners, [("home", false), ("office", true)]);
    let log = log_copy(&after, &JobId("run-office".into()), &redactor)
        .await
        .unwrap();
    assert_eq!(String::from_utf8(log).unwrap(), "step one\npassword=***\n");
    assert_eq!(
        log_copy(&after, &JobId("late-office".into()), &redactor).await,
        Err(DataError::NotFound)
    );
}
