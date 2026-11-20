use crate::common::TempDir;
use crate::kit::{alive, Gate, Mode, Release};
use crate::support::*;
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_core::JobState;
use lr_proto::{QueueEvent, ReasonCode};

fn helper_profile(name: &str, mode: Mode, gate: &Gate, timeout_s: u64) -> String {
    format!(
        "[profile.{name}]\ncommand = {}\ntimeout_s = {timeout_s}\nmem_mb = 256\n",
        toml_str(&gate.command(mode, name))
    )
}

// scenario: 16.21
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kill_runner_mid_job_is_noticed_and_the_job_waits_for_its_deadline() {
    let dir = TempDir::new("e2e-lost");
    let gate = Gate::new().await;
    let release = Release::new(&[&gate]);
    let layout = layout(&dir, "machine", "[grace]\nlost_min = 1\n");
    add_runner(&layout, "r1");
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runner = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("r1 starts");
    runner_online(&layout, "r1").await;

    let repo = repo(&dir, &helper_profile("held", Mode::Wait, &gate, 10));
    let mut events = broker.broker_events().expect("broker events");
    let (held_client, held) = submit(&layout, &repo, "held", None).await;
    broker_event(&mut events, "the held job to start", |e| match e {
        QueueEvent::JobStarted { job, .. } if *job == held => Some(()),
        _ => None,
    })
    .await;
    gate.started("held", "child").await;

    runner.stop();
    broker_event(
        &mut events,
        "the killed runner to go offline",
        |e| match e {
            QueueEvent::RunnerOffline { name } if name.ends_with("r1") => Some(()),
            _ => None,
        },
    )
    .await;
    let view = status(&layout).await.expect("status");
    let row = view.running.iter().find(|r| r.summary.job == held);
    assert!(
        row.is_some_and(|r| r.summary.state == JobState::Running),
        "the job stays running until timeout_s + lost_min: {view:?}"
    );

    held_client.close().await;
    hub_role.stop();
    broker.stop();
    drop(release);
    remove(dir).await;
}

// scenario: 16.20
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn broker_restart_keeps_queue() {
    let dir = TempDir::new("e2e-restart");
    let gate = Gate::new().await;
    let release = Release::new(&[&gate]);
    let layout = layout(&dir, "machine", "");
    add_runner(&layout, "r1");
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runner = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    runner_online(&layout, "r1").await;

    let profiles = format!(
        "{}\n[profile.next]\ncommand = \"echo queued-before-restart\"\ntimeout_s = 60\nmem_mb = 256\n",
        helper_profile("first", Mode::Wait, &gate, 120)
    );
    let repo = repo(&dir, &profiles);
    let (first_client, first) = submit(&layout, &repo, "first", None).await;
    gate.started("first", "child").await;
    let mut events = broker.broker_events().expect("broker events");
    let (next_client, next) = submit(&layout, &repo, "next", None).await;
    broker_event(&mut events, "the next job queued", |e| match e {
        QueueEvent::JobQueued { job } if job.job == next => Some(()),
        _ => None,
    })
    .await;

    broker.stop();
    let broker = start_broker(&layout, false)
        .await
        .expect("the broker starts again on the same state");
    runner_online(&layout, "r1").await;
    let view = until_hub(&layout, "the restored queue", || async {
        let v = status(&layout).await?;
        let queued = v.queue.iter().any(|q| q.summary.job == next);
        let running = v.running.iter().any(|r| r.summary.job == first);
        (queued && running).then_some(v)
    })
    .await;
    assert_eq!(view.queue.len(), 1, "{view:?}");

    gate.open();
    let a = until_exit(first_client, &first, HANG).await;
    assert_eq!(a.state, JobState::Passed, "{a:?}");
    let b = until_exit(next_client, &next, HANG).await;
    assert_eq!(b.state, JobState::Passed, "{b:?}");
    assert!(b.output.contains("queued-before-restart"), "{}", b.output);
    let rows = history(&layout).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows.iter().all(|r| r.state == JobState::Passed), "{rows:?}");

    runner.stop();
    hub_role.stop();
    broker.stop();
    drop(release);
    remove(dir).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_with_a_reason_ends_the_job_and_writes_the_reason_into_its_log() {
    let dir = TempDir::new("e2e-cancel");
    let gate = Gate::new().await;
    let release = Release::new(&[&gate]);
    let layout = layout(&dir, "machine", "");
    add_runner(&layout, "r1");
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runner = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    runner_online(&layout, "r1").await;

    let repo = repo(&dir, &helper_profile("held", Mode::Wait, &gate, 120));
    let (client, job) = submit(&layout, &repo, "held", None).await;
    gate.started("held", "child").await;
    let reason = "superseded by a newer push";
    let code = lr_cli::commands::jobs::cancel(
        &layout,
        Some(job.0.clone()),
        LABEL,
        false,
        false,
        Some(reason.into()),
    )
    .await
    .expect("cancel");
    assert_eq!(code, 0);

    let ended = until_exit(client, &job, HANG).await;
    assert_eq!(ended.state, JobState::Cancelled, "{ended:?}");
    assert_eq!(ended.reason, Some(ReasonCode::CancelledByUser), "{ended:?}");
    let log = std::fs::read_to_string(
        repo.join("temp")
            .join("runner")
            .join(format!("{}.log", job.0)),
    )
    .expect("log copy");
    let line = log
        .lines()
        .find(|l| l.starts_with("=== cancel requested by "))
        .unwrap_or_else(|| panic!("no cancel line in {log}"));
    assert!(
        line.ends_with(&format!("(cancelled_by_user): {reason} ===")),
        "{line}"
    );
    let rows = history(&layout).await;
    let row = rows.iter().find(|r| r.job == job).expect("history row");
    assert_eq!(row.state, JobState::Cancelled);
    assert_eq!(row.reason, Some(ReasonCode::CancelledByUser));

    runner.stop();
    hub_role.stop();
    broker.stop();
    drop(release);
    remove(dir).await;
}

// scenario: 16.21
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timeout_kills_grandchildren() {
    let dir = TempDir::new("e2e-timeout");
    let gate = Gate::new().await;
    let release = Release::new(&[&gate]);
    let layout = layout(&dir, "machine", "");
    add_runner(&layout, "r1");
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runner = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    runner_online(&layout, "r1").await;

    let timeout_s = 12;
    let repo = repo(&dir, &helper_profile("tree", Mode::Tree, &gate, timeout_s));
    let (client, job) = submit(&layout, &repo, "tree", None).await;
    let grandchild = gate.started("tree", "grandchild").await;
    let child = gate.started("tree", "child").await;
    let tree = [child.process(), grandchild.process()];
    assert!(
        tree.iter().all(|&p| alive(p)),
        "both helpers run before the timeout"
    );

    let limit = HANG + std::time::Duration::from_secs(timeout_s);
    let ended = until_exit(client, &job, limit).await;
    assert_eq!(ended.state, JobState::Timeout, "{ended:?}");
    assert!(ended.output.contains("child-started"), "{}", ended.output);
    child.gone("the child to be killed").await;
    grandchild.gone("the grandchild to be killed").await;

    runner.stop();
    hub_role.stop();
    broker.stop();
    drop(release);
    remove(dir).await;
}
