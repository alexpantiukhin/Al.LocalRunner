use crate::common::{self, TempDir};
use crate::helper::spawn_waiting;
use crate::kit::*;
use crate::support::*;
use lr_cli::roles::start_hub;
use lr_core::JobState;
use lr_exec::process_start;
use lr_proto::ReasonCode;
use std::path::Path;
use std::time::Duration;

const ENDS: Duration = Duration::from_secs(90);

fn diagnostics_mention(root: &Path, text: &str, job: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(root.join("logs")) else {
        return false;
    };
    entries.flatten().any(|e| {
        std::fs::read_to_string(e.path())
            .is_ok_and(|t| t.lines().any(|l| l.contains(text) && l.contains(job)))
    })
}

// scenario: 16.18
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dead_sender_job_is_cancelled_and_detached_job_survives() {
    let dir = TempDir::new("e2e-dead");
    let gate = dir.path().join("gate");
    let sender_gate = dir.path().join("sender-gate");
    let mut release = Release::new(&[&gate, &sender_gate]);
    let (main, roles) = machine(&dir, "main", "", &["r1", "r2"], &runner_toml()).await;
    let profiles = format!(
        "{}\n{}\n{}\n{}",
        held_profile("attached", dir.path(), &gate, 120, "mem_mb = 256\n"),
        held_profile("detached", dir.path(), &gate, 120, "mem_mb = 256\n"),
        echo_profile("waiting", "never-runs", 60, "mem_mb = 256\n"),
        held_profile("unknown", dir.path(), &gate, 120, "mem_mb = 256\n"),
    );
    let repo = repo(&dir, &profiles);

    let sender_pids = dir.path().join("sender.pid");
    let mut sender = spawn_waiting(&sender_pids, &sender_gate);
    let pid = helper_pid(&sender_pids, "child").await;
    let mut identity = common::session(LABEL, None, &repo).expect("identity");
    identity.sender_pid = pid;
    identity.sender_start = Some(process_start(pid).expect("the sender runs"));
    let mut unknown_start = identity.clone();
    unknown_start.sender_start = None;

    let (attached_client, attached) =
        submit_as(&main, identity.clone(), "attached", None, false).await;
    release.watch(started(dir.path(), "attached", "child").await);
    let (detached_client, detached) =
        submit_as(&main, identity.clone(), "detached", None, true).await;
    release.watch(started(dir.path(), "detached", "child").await);
    let (waiting_client, waiting) = submit_as(&main, identity, "waiting", None, false).await;
    queued(&main, &waiting).await;
    let (unknown_client, unknown) = submit_as(&main, unknown_start, "unknown", None, false).await;
    queued(&main, &unknown).await;

    sender.kill().unwrap();
    sender.wait().unwrap();
    let a = until_exit(attached_client, &attached, ENDS).await;
    assert_eq!(a.state, JobState::Cancelled, "{a:?}");
    assert_eq!(a.reason, Some(ReasonCode::SenderGone), "{a:?}");
    let w = until_exit(waiting_client, &waiting, ENDS).await;
    assert_eq!(w.state, JobState::Cancelled, "{w:?}");
    assert_eq!(w.reason, Some(ReasonCode::SenderGone), "{w:?}");

    release.watch(started(dir.path(), "unknown", "child").await);
    let view = status(&main).await.expect("status");
    for job in [&detached, &unknown] {
        assert!(
            view.running.iter().any(|r| &r.summary.job == job),
            "{job} lives on: {view:?}"
        );
    }
    eventually(
        "the warning about the unknown start time",
        Duration::from_secs(30),
        || async {
            diagnostics_mention(&main.root, "submitter start time unknown", &unknown.0)
                .then_some(())
        },
    )
    .await;

    open(&gate);
    let d = until_exit(detached_client, &detached, ENDS).await;
    assert_eq!(d.state, JobState::Passed, "{d:?}");
    let u = until_exit(unknown_client, &unknown, ENDS).await;
    assert_eq!(u.state, JobState::Passed, "{u:?}");

    roles.stop();
    drop(release);
    remove(dir).await;
}

// scenario: 16.19
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_restart_resumes_results_without_duplicates() {
    let dir = TempDir::new("e2e-hubr");
    let gate = dir.path().join("gate");
    let mut release = Release::new(&[&gate]);
    let (main, mut roles) = machine(&dir, "main", "", &["r1"], &runner_toml()).await;
    let repo = repo(
        &dir,
        &held_profile("asleep", dir.path(), &gate, 120, "mem_mb = 256\n"),
    );
    let (client, job) = submit(&main, &repo, "asleep", None).await;
    let helper = started(dir.path(), "asleep", "child").await;
    release.watch(helper);
    let copy = repo
        .join("temp")
        .join("runner")
        .join(format!("{}.log", job.0));
    eventually("the first line in the hub's copy", ENDS, || async {
        std::fs::read_to_string(&copy)
            .is_ok_and(|t| t.contains("child-started"))
            .then_some(())
    })
    .await;

    client.close().await;
    roles.take_hub().stop();
    open(&gate);
    gone("the job to end while the hub is away", &[helper]).await;
    let hub = eventually("the hub to start again", ENDS, || async {
        start_hub(&main, false).await.ok()
    })
    .await;
    roles.push(hub);

    let done = row(&main, &job).await;
    assert_eq!(done.state, JobState::Passed, "the order went on: {done:?}");
    let rows = history(&main).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let log = eventually("the result in the hub's copy", ENDS, || async {
        std::fs::read_to_string(&copy)
            .ok()
            .filter(|t| t.contains("child-done"))
    })
    .await;
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
