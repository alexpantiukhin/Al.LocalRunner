use lr_abstractions::fakes::{channel_pair, ManualClock};
use lr_abstractions::{Frame, Peer, PeerAccess, Transport};
use lr_cli::dashboard::{build_snapshot, detail_of};
use lr_cli::ipc_client::IpcClient;
use lr_core::{Endpoint, JobId, JobState, StepState, Timestamp, Zone};
use lr_obs::Redactor;
use lr_proto::{
    BrokerStatus, CliReply, CliRequest, DashboardList, DashboardSnapshot, IpcChannel, IpcOpenReply,
    JobDetail, JobSummary, PoolLoad, QueueItem, ReadReply, ReadRequest, RunningView, StatsView,
    StatusFilter, StatusView, StepView, WireSource, PROTOCOL_VERSION,
};
use std::sync::Arc;

type Answer = Box<dyn Fn(&ReadRequest) -> Option<ReadReply> + Send>;

async fn fake_hub(answer: Answer) -> IpcClient {
    let peer = || Peer {
        endpoint: Endpoint::Ipc {
            os_user: "alex".into(),
        },
        key: None,
        access: PeerAccess::Local,
    };
    let clock = Arc::new(ManualClock::new(Timestamp(0)));
    let (cli, hub) = channel_pair(clock, peer(), peer());
    tokio::spawn(async move {
        let _hello = hub.recv().await.expect("hello");
        let ready = IpcOpenReply::Ready {
            version: PROTOCOL_VERSION,
        };
        hub.send(Frame::json(&ready).unwrap()).await.unwrap();
        while let Ok(frame) = hub.recv().await {
            let req: CliRequest = lr_proto::decode(frame.as_bytes()).unwrap();
            let reply = match req {
                CliRequest::Brokers => CliReply::Brokers {
                    brokers: vec![BrokerStatus {
                        name: "office".into(),
                        zone: Some(Zone::Lan),
                        available: true,
                        outbox: 0,
                        since: None,
                    }],
                },
                CliRequest::Orders => CliReply::Orders {
                    orders: Vec::new(),
                    truncated: false,
                },
                CliRequest::Read(r) => CliReply::Read {
                    broker: "office".into(),
                    reply: answer(&r.request).unwrap_or(ReadReply::Unknown),
                },
                other => panic!("unexpected {other:?}"),
            };
            hub.send(Frame::json(&reply).unwrap()).await.unwrap();
        }
    });
    IpcClient::open(Box::new(cli), IpcChannel::Cli)
        .await
        .expect("the fake hub opens the channel")
}

fn summary(job: &str, state: JobState, mine: bool) -> JobSummary {
    JobSummary {
        job: JobId(job.into()),
        client_label: "laptop".into(),
        state,
        platform: Some("linux".into()),
        runner: Some("r1".into()),
        queued_at: Timestamp(1_000),
        started_at: Some(Timestamp(2_000)),
        finished_at: None,
        exit_code: None,
        reason: None,
        project: mine.then(|| "acme/app".into()),
        label: mine.then(|| "agent".into()),
        profile: mine.then(|| "unit".into()),
        source: mine.then_some(WireSource::Github),
        note: mine.then(|| "flaky emulator".into()),
        branch: mine.then(|| "feat/x".into()),
        cwd: mine.then(|| "/work/app".into()),
        timeout_s: mine.then_some(600),
        github_run_url: mine.then(|| "https://github.com/acme/app/actions/runs/7".into()),
        peak_mem_mb: None,
        peak_disk_mb: None,
        labels: Vec::new(),
    }
}

fn step(ord: u32, state: StepState, started: Option<u64>) -> StepView {
    StepView {
        ord,
        name: format!("s{ord}"),
        state,
        exit_code: None,
        started_at: started.map(Timestamp),
        finished_at: None,
        timeout_s: 60,
    }
}

fn status() -> StatusView {
    StatusView {
        running: vec![
            RunningView {
                summary: summary("mine", JobState::Running, true),
                steps: vec![step(0, StepState::Running, Some(2_000))],
                last_lines: Vec::new(),
            },
            RunningView {
                summary: summary("theirs", JobState::Running, false),
                steps: Vec::new(),
                last_lines: Vec::new(),
            },
        ],
        queue: vec![QueueItem {
            summary: summary("waiting", JobState::Queued, true),
            position: 1,
            wait_reason: Some("needs memory".into()),
            wait_code: Some("memory".into()),
        }],
        recent: Vec::new(),
        pool: PoolLoad {
            used_mem_mb: 0,
            total_mem_mb: 1,
            used_cpu: 0,
            total_cpu: 1,
        },
        next: None,
    }
}

fn finished_detail() -> JobDetail {
    let mut s = summary("done", JobState::Cancelled, true);
    s.finished_at = Some(Timestamp(9_000));
    let mut d = detail_of(s, vec![step(0, StepState::Cancelled, Some(2_000))], None);
    d.cancelled_by = Some("dashboard:alex".into());
    d
}

async fn snapshot(details: bool) -> DashboardSnapshot {
    let client = fake_hub(Box::new(move |req| match req {
        ReadRequest::Status(_) => Some(ReadReply::Status(status())),
        ReadRequest::HistoryDetails(_) if details => Some(ReadReply::Details {
            jobs: vec![finished_detail()],
            next: None,
        }),
        ReadRequest::History(_) => Some(ReadReply::History {
            jobs: vec![finished_detail().summary],
            next: None,
        }),
        ReadRequest::Runners => Some(ReadReply::Runners { runners: vec![] }),
        ReadRequest::Stats(_) => Some(ReadReply::Stats(StatsView {
            profiles: Vec::new(),
            clients: Vec::new(),
        })),
        _ => None,
    }))
    .await;
    build_snapshot(&client, &StatusFilter::default(), true, &Redactor::new(&[]))
        .await
        .unwrap()
}

fn find<'a>(snap: &'a DashboardSnapshot, job: &str) -> &'a JobDetail {
    &snap
        .jobs
        .iter()
        .find(|j| j.detail.summary.job.0 == job)
        .unwrap()
        .detail
}

#[test]
fn a_card_from_a_summary_carries_the_identity_fields_the_page_reads() {
    let d = detail_of(summary("mine", JobState::Queued, true), Vec::new(), None);
    assert_eq!(d.source, Some(WireSource::Github));
    assert_eq!(d.cwd.as_deref(), Some("/work/app"));
    assert_eq!(d.note.as_deref(), Some("flaky emulator"));
    assert_eq!(d.branch.as_deref(), Some("feat/x"));
    assert_eq!(
        d.github_run_url.as_deref(),
        Some("https://github.com/acme/app/actions/runs/7")
    );
    let hidden = detail_of(summary("theirs", JobState::Queued, false), Vec::new(), None);
    assert!(hidden.source.is_none() && hidden.cwd.is_none() && hidden.note.is_none());
    assert!(hidden.branch.is_none() && hidden.github_run_url.is_none());
}

#[tokio::test]
async fn the_snapshot_json_has_identity_fields_steps_and_the_canceller_of_history() {
    let snap = snapshot(true).await;
    let mine = find(&snap, "mine");
    assert_eq!(mine.source, Some(WireSource::Github));
    assert_eq!(mine.cwd.as_deref(), Some("/work/app"));
    assert_eq!(mine.steps[0].state, StepState::Running);
    assert_eq!(mine.steps[0].started_at, Some(Timestamp(2_000)));
    let waiting = snap
        .jobs
        .iter()
        .find(|j| j.detail.summary.job.0 == "waiting")
        .unwrap();
    assert_eq!(waiting.wait_code.as_deref(), Some("memory"));
    assert_eq!(waiting.detail.wait_reason.as_deref(), Some("needs memory"));
    let theirs = find(&snap, "theirs");
    assert!(theirs.source.is_none() && theirs.cwd.is_none() && theirs.note.is_none());
    let done = find(&snap, "done");
    assert_eq!(done.cancelled_by.as_deref(), Some("dashboard:alex"));
    assert_eq!(done.steps.len(), 1);
    assert_eq!(done.note.as_deref(), Some("flaky emulator"));
    let json = serde_json::to_value(&snap).unwrap();
    let row = json["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["detail"]["summary"]["job"] == "mine")
        .unwrap();
    assert_eq!(row["detail"]["source"], "github");
    assert_eq!(row["detail"]["cwd"], "/work/app");
    assert_eq!(row["detail"]["summary"]["timeout_s"], 600);
}

#[tokio::test]
async fn an_older_broker_without_details_still_lists_its_history_with_fields() {
    let snap = snapshot(false).await;
    let done = snap
        .jobs
        .iter()
        .find(|j| j.detail.summary.job.0 == "done")
        .unwrap();
    assert_eq!(done.list, DashboardList::History);
    assert_eq!(done.detail.cwd.as_deref(), Some("/work/app"));
    assert!(done.detail.steps.is_empty());
}
