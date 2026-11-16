use crate::common::TempDir;
use crate::helper::{helper_command, Mode};
use crate::support::*;
use lr_abstractions::{Liveness, ProcessInspector};
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_core::JobState;
use lr_exec::{process_start, OsProcessInspector};
use lr_proto::{QueueEvent, ReasonCode};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn helper_profile(name: &str, mode: Mode, dir: &TempDir, timeout_s: u64) -> String {
    let pids = dir.path().join(format!("{name}.pid"));
    let gate = dir.path().join("gate");
    format!(
        "[profile.{name}]\ncommand = {}\ntimeout_s = {timeout_s}\nmem_mb = 256\n",
        toml_str(&helper_command(mode, &pids, &gate))
    )
}

fn pids(dir: &TempDir, name: &str) -> std::path::PathBuf {
    dir.path().join(format!("{name}.pid"))
}

fn open_gate(dir: &TempDir) {
    std::fs::write(dir.path().join("gate"), b"open").unwrap();
}

/// Opens the gate of the helpers when the test ends, even by a panic, and waits until the job
/// processes it watches are gone, so none of them holds the temporary directory.
struct Release {
    dir: PathBuf,
    watched: Vec<(u32, u64)>,
}

impl Release {
    fn new(dir: &TempDir) -> Release {
        Release {
            dir: dir.path().to_path_buf(),
            watched: Vec::new(),
        }
    }

    fn watch(&mut self, pid: u32) {
        if let Some(start) = process_start(pid) {
            self.watched.push((pid, start));
        }
    }
}

impl Drop for Release {
    fn drop(&mut self) {
        let _ = std::fs::write(self.dir.join("gate"), b"open");
        let started = Instant::now();
        while self.watched.iter().any(|&(p, s)| alive(p, s))
            && started.elapsed() < Duration::from_secs(20)
        {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

// scenario: 16.21
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kill_runner_mid_job_is_noticed_and_the_job_waits_for_its_deadline() {
    let dir = TempDir::new("e2e-lost");
    let mut release = Release::new(&dir);
    let layout = layout(&dir, "machine", "[grace]\nlost_min = 1\n");
    add_runner(&layout, "r1");
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runner = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("r1 starts");
    runner_online(&layout, "r1").await;

    let repo = repo(&dir, &helper_profile("held", Mode::Wait, &dir, 10));
    let mut events = broker.broker_events().expect("broker events");
    let (held_client, held) = submit(&layout, &repo, "held", None).await;
    broker_event(&mut events, "the held job to start", |e| match e {
        QueueEvent::JobStarted { job, .. } if *job == held => Some(()),
        _ => None,
    })
    .await;
    release.watch(helper_pid(&pids(&dir, "held"), "child").await);

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
    let mut release = Release::new(&dir);
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
        helper_profile("first", Mode::Wait, &dir, 120)
    );
    let repo = repo(&dir, &profiles);
    let (first_client, first) = submit(&layout, &repo, "first", None).await;
    release.watch(helper_pid(&pids(&dir, "first"), "child").await);
    let (next_client, next) = submit(&layout, &repo, "next", None).await;
    eventually("the next job queued", Duration::from_secs(30), || async {
        let v = status(&layout).await?;
        v.queue.iter().any(|q| q.summary.job == next).then_some(())
    })
    .await;

    broker.stop();
    let broker = start_broker(&layout, false)
        .await
        .expect("the broker starts again on the same state");
    runner_online(&layout, "r1").await;
    let view = eventually("the restored queue", Duration::from_secs(30), || async {
        let v = status(&layout).await?;
        let queued = v.queue.iter().any(|q| q.summary.job == next);
        let running = v.running.iter().any(|r| r.summary.job == first);
        (queued && running).then_some(v)
    })
    .await;
    assert_eq!(view.queue.len(), 1, "{view:?}");

    open_gate(&dir);
    let a = until_exit(first_client, &first, Duration::from_secs(60)).await;
    assert_eq!(a.state, JobState::Passed, "{a:?}");
    let b = until_exit(next_client, &next, Duration::from_secs(60)).await;
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
    let mut release = Release::new(&dir);
    let layout = layout(&dir, "machine", "");
    add_runner(&layout, "r1");
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runner = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    runner_online(&layout, "r1").await;

    let repo = repo(&dir, &helper_profile("held", Mode::Wait, &dir, 120));
    let (client, job) = submit(&layout, &repo, "held", None).await;
    release.watch(helper_pid(&pids(&dir, "held"), "child").await);
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

    let ended = until_exit(client, &job, Duration::from_secs(60)).await;
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

fn alive(pid: u32, start: u64) -> bool {
    OsProcessInspector.liveness(pid, start) == Liveness::Alive
}

// scenario: 16.21
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timeout_kills_grandchildren() {
    let dir = TempDir::new("e2e-timeout");
    let mut release = Release::new(&dir);
    let layout = layout(&dir, "machine", "");
    add_runner(&layout, "r1");
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runner = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    runner_online(&layout, "r1").await;

    let repo = repo(&dir, &helper_profile("tree", Mode::Tree, &dir, 20));
    let (client, job) = submit(&layout, &repo, "tree", None).await;
    let file = pids(&dir, "tree");
    let grandchild = helper_pid(&file, "grandchild").await;
    let child = helper_pid(&file, "child").await;
    let tree: Vec<(u32, u64)> = [child, grandchild]
        .into_iter()
        .filter_map(|pid| process_start(pid).map(|s| (pid, s)))
        .collect();
    assert_eq!(tree.len(), 2, "both helpers run before the timeout");
    assert!(tree.iter().all(|&(p, s)| alive(p, s)));
    release.watched.extend(tree.iter().copied());

    let ended = until_exit(client, &job, Duration::from_secs(60)).await;
    assert_eq!(ended.state, JobState::Timeout, "{ended:?}");
    assert!(ended.output.contains("child-started"), "{}", ended.output);
    eventually(
        "the whole tree to be gone",
        Duration::from_secs(10),
        || async { tree.iter().all(|&(p, s)| !alive(p, s)).then_some(()) },
    )
    .await;

    runner.stop();
    hub_role.stop();
    broker.stop();
    drop(release);
    remove(dir).await;
}
