mod common;

use common::TempDir;
use lr_abstractions::fakes::{channel_pair, ManualClock};
use lr_abstractions::{DataError, Frame, Peer, PeerAccess, Transport};
use lr_cli::dashboard::{build_snapshot, keep_last_known, log_copy};
use lr_cli::ipc_client::IpcClient;
use lr_core::{Endpoint, JobId, JobState, Timestamp, Zone};
use lr_obs::Redactor;
use lr_proto::{
    BrokerStatus, CliReply, CliRequest, DashboardList, DashboardSnapshot, HubOrder, IpcChannel,
    IpcOpenReply, JobSummary, PoolLoad, ProfileStatsView, QueueItem, ReadReply, ReadRequest,
    RunningView, SpreadView, StatsView, StatusFilter, StatusView, PROTOCOL_VERSION,
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
    }
}

fn broker(name: &str, available: bool) -> BrokerStatus {
    BrokerStatus {
        name: name.into(),
        zone: Some(Zone::Lan),
        available,
        outbox: 0,
    }
}

fn status(name: &str) -> StatusView {
    StatusView {
        running: vec![RunningView {
            summary: summary(&format!("run-{name}"), JobState::Running),
            steps: Vec::new(),
            last_lines: vec![format!("{name} api_token=hunter2hunter2")],
        }],
        queue: vec![QueueItem {
            summary: summary(&format!("wait-{name}"), JobState::Queued),
            position: 1,
            wait_reason: Some(format!("runner of {name} busy")),
        }],
        recent: Vec::new(),
        pool: PoolLoad {
            used_mem_mb: 512,
            total_mem_mb: 4096,
            used_cpu: 1,
            total_cpu: 4,
        },
        next: None,
    }
}

fn stats(profile: &str) -> StatsView {
    let spread = SpreadView {
        median: None,
        p95: None,
    };
    StatsView {
        profiles: vec![ProfileStatsView {
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
            let reply = match &r.request {
                ReadRequest::Status(_) => ReadReply::Status(status(&name)),
                ReadRequest::History(_) => ReadReply::History {
                    jobs: vec![
                        summary(&format!("done-{name}"), JobState::Passed),
                        summary(&format!("still-{name}"), JobState::Running),
                    ],
                    next: None,
                },
                ReadRequest::Runners => ReadReply::Runners { runners: vec![] },
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
