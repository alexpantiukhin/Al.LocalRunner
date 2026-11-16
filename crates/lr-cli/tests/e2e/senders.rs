use crate::common::{self, TempDir};
use crate::helper::spawn_sender;
use crate::kit::*;
use crate::support::*;
use lr_cli::roles::start_hub;
use lr_core::JobState;
use lr_exec::process_start;
use lr_proto::{QueueEvent, ReasonCode};

// scenario: 16.18
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dead_sender_job_is_cancelled_and_detached_job_survives() {
    let dir = TempDir::new("e2e-dead");
    let gate = Gate::new().await;
    let release = Release::new(&[&gate]);
    let (main, roles) = machine(&dir, "main", "", &["r1", "r2"], &runner_toml()).await;
    let profiles = format!(
        "{}\n{}\n{}\n{}",
        held_profile("attached", &gate, 120, "mem_mb = 256\n"),
        held_profile("detached", &gate, 120, "mem_mb = 256\n"),
        echo_profile("waiting", "never-runs", 60, "mem_mb = 256\n"),
        held_profile("unknown", &gate, 120, "mem_mb = 256\n"),
    );
    let repo = repo(&dir, &profiles);

    let mut sender = spawn_sender();
    let pid = sender.id();
    let mut identity = common::session(LABEL, None, &repo).expect("identity");
    identity.sender_pid = pid;
    identity.sender_start = Some(process_start(pid).expect("the sender runs"));
    let mut unknown_start = identity.clone();
    unknown_start.sender_start = None;

    let (attached_client, attached) =
        submit_as(&main, identity.clone(), "attached", None, false).await;
    gate.started("attached", "child").await;
    let (detached_client, detached) =
        submit_as(&main, identity.clone(), "detached", None, true).await;
    gate.started("detached", "child").await;
    let (waiting_client, waiting) = submit_as(&main, identity, "waiting", None, false).await;
    queued(&main, &waiting).await;
    let (unknown_client, unknown) = submit_as(&main, unknown_start, "unknown", None, false).await;
    queued(&main, &unknown).await;

    sender.kill().unwrap();
    sender.wait().unwrap();
    let a = until_exit(attached_client, &attached, HANG).await;
    assert_eq!(a.state, JobState::Cancelled, "{a:?}");
    assert_eq!(a.reason, Some(ReasonCode::SenderGone), "{a:?}");
    let w = until_exit(waiting_client, &waiting, HANG).await;
    assert_eq!(w.state, JobState::Cancelled, "{w:?}");
    assert_eq!(w.reason, Some(ReasonCode::SenderGone), "{w:?}");

    gate.started("unknown", "child").await;
    let view = status(&main).await.expect("status");
    for job in [&detached, &unknown] {
        assert!(
            view.running.iter().any(|r| &r.summary.job == job),
            "{job} lives on: {view:?}"
        );
    }
    roles.flush();
    let warned = std::fs::read_dir(main.root.join("logs"))
        .expect("the broker's diagnostics")
        .flatten()
        .any(|e| {
            std::fs::read_to_string(e.path()).is_ok_and(|t| {
                t.lines()
                    .any(|l| l.contains("submitter start time unknown") && l.contains(&unknown.0))
            })
        });
    assert!(warned, "the warning about the unknown start time");

    gate.open();
    let d = until_exit(detached_client, &detached, HANG).await;
    assert_eq!(d.state, JobState::Passed, "{d:?}");
    let u = until_exit(unknown_client, &unknown, HANG).await;
    assert_eq!(u.state, JobState::Passed, "{u:?}");

    roles.stop();
    drop(release);
    remove(dir).await;
}

// scenario: 16.19
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_restart_resumes_results_without_duplicates() {
    let dir = TempDir::new("e2e-hubr");
    let gate = Gate::new().await;
    let release = Release::new(&[&gate]);
    let (main, mut roles) = machine(&dir, "main", "", &["r1"], &runner_toml()).await;
    let repo = repo(&dir, &held_profile("asleep", &gate, 120, "mem_mb = 256\n"));
    let (client, job) = submit(&main, &repo, "asleep", None).await;
    let helper = gate.started("asleep", "child").await;
    until_output(&client, &job, "child-started").await;
    let copy = repo
        .join("temp")
        .join("runner")
        .join(format!("{}.log", job.0));

    client.close().await;
    let mut events = roles.broker_events();
    roles.take_hub().stopped().await;
    gate.open();
    helper.gone("the job to end while the hub is away").await;
    broker_event(
        &mut events,
        "the job to end while the hub is away",
        |e| match e {
            QueueEvent::JobFinished { job: j, .. } if *j == job => Some(()),
            _ => None,
        },
    )
    .await;
    roles.push(start_hub(&main, false).await.expect("the hub starts again"));

    let watch = follow(&main, &job).await;
    let done = until_exit(watch, &job, HANG).await;
    assert_eq!(done.state, JobState::Passed, "the order went on: {done:?}");
    let rows = history(&main).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let log = std::fs::read_to_string(&copy).expect("the hub's copy");
    for line in ["child-started", "child-done"] {
        assert_eq!(
            log.matches(line).count(),
            1,
            "resume by seq drops duplicates: {log}"
        );
    }

    roles.stop();
    drop(release);
    remove(dir).await;
}
