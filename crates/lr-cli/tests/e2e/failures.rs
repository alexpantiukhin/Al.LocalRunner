use crate::common::TempDir;
use crate::helper::{helper_command, Mode};
use crate::support::*;
use lr_abstractions::{Liveness, ProcessInspector};
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_core::JobState;
use lr_exec::{process_start, OsProcessInspector};
use lr_proto::ReasonCode;
use std::path::Path;
use std::time::Duration;

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

struct GateOnDrop<'a>(&'a Path);

impl Drop for GateOnDrop<'_> {
    fn drop(&mut self) {
        let _ = std::fs::write(self.0.join("gate"), b"open");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kill_runner_mid_job_marks_lost_after_deadline() {
    let dir = TempDir::new("e2e-lost");
    let _gate = GateOnDrop(dir.path());
    let layout = layout(
        &dir,
        "machine",
        "[grace]\nlost_min = 1\n\n[pool]\nmem_mb = 256\n",
    );
    add_runner(&layout, "r1");
    add_runner(&layout, "r2");
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let first = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("r1 starts");
    runner_online(&layout, "r1").await;

    let profiles = format!(
        "{}\n[profile.quick]\ncommand = \"echo after-the-loss\"\ntimeout_s = 60\nmem_mb = 256\n",
        helper_profile("held", Mode::Wait, &dir, 10)
    );
    let repo = repo(&dir, &profiles);
    let (held_client, held) = submit(&layout, &repo, "held", None).await;
    helper_pid(&pids(&dir, "held"), "child").await;
    let second = start_runners(&layout, Some("r2"), 1, None, false)
        .await
        .expect("r2 starts");
    runner_online(&layout, "r2").await;
    let (quick_client, quick) = submit(&layout, &repo, "quick", None).await;
    let view = eventually(
        "the quick job to wait for the pool",
        Duration::from_secs(30),
        || async {
            let v = status(&layout).await?;
            v.queue.iter().any(|q| q.summary.job == quick).then_some(v)
        },
    )
    .await;
    assert_eq!(view.pool.used_mem_mb, 256, "{view:?}");

    first.stop();
    let lost = until_exit(held_client, &held, Duration::from_secs(160)).await;
    assert_eq!(lost.state, JobState::Lost, "{lost:?}");
    assert_eq!(lost.reason, Some(ReasonCode::RunnerLost), "{lost:?}");
    let rows = history(&layout).await;
    let row = rows.iter().find(|r| r.job == held).expect("held row");
    assert_eq!(row.state, JobState::Lost);
    assert_eq!(row.reason, Some(ReasonCode::RunnerLost));
    let finished = row.finished_at.expect("lost has an end").0;
    let started = row.started_at.expect("it was started").0;
    assert!(
        finished >= started + 70_000,
        "lost only after timeout_s + lost_min: {row:?}"
    );

    let done = until_exit(quick_client, &quick, Duration::from_secs(60)).await;
    assert_eq!(
        done.state,
        JobState::Passed,
        "the pool memory came back: {done:?}"
    );
    assert!(done.output.contains("after-the-loss"), "{}", done.output);
    let view = status(&layout).await.expect("status");
    assert_eq!(view.pool.used_mem_mb, 0, "{view:?}");

    second.stop();
    hub_role.stop();
    broker.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn broker_restart_keeps_queue() {
    let dir = TempDir::new("e2e-restart");
    let _gate = GateOnDrop(dir.path());
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
    helper_pid(&pids(&dir, "first"), "child").await;
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
}

fn alive(pid: u32, start: u64) -> bool {
    OsProcessInspector.liveness(pid, start) == Liveness::Alive
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timeout_kills_grandchildren() {
    let dir = TempDir::new("e2e-timeout");
    let _gate = GateOnDrop(dir.path());
    let layout = layout(&dir, "machine", "");
    add_runner(&layout, "r1");
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runner = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    runner_online(&layout, "r1").await;

    let repo = repo(&dir, &helper_profile("tree", Mode::Tree, &dir, 5));
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
}
