use crate::common::TempDir;
use crate::kit::*;
use crate::support::*;
use lr_core::JobState;
use std::time::Duration;

const ENDS: Duration = Duration::from_secs(90);

// scenario: 16.14
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn small_order_fills_the_hole_without_delaying_the_head() {
    let dir = TempDir::new("e2e-hole");
    let gate = dir.path().join("gate");
    let mut release = Release::new(&[&gate]);
    let (main, roles) = machine(
        &dir,
        "main",
        "[pool]\nmem_mb = 1024\n",
        &["r1", "r2", "r3"],
        &runner_toml(),
    )
    .await;
    let profiles = format!(
        "{}\n{}\n{}\n{}",
        held_profile("running", dir.path(), &gate, 120, "mem_mb = 512\n"),
        echo_profile("heavy", "heavy-ran", 60, "mem_mb = 1024\n"),
        echo_profile("small", "small-filled-the-hole", 20, "mem_mb = 256\n"),
        echo_profile("long", "long-ran-last", 600, "mem_mb = 256\n"),
    );
    let repo = repo(&dir, &profiles);

    let (running_client, running) = submit(&main, &repo, "running", None).await;
    release.watch(started(dir.path(), "running", "child").await);
    let (heavy_client, heavy) = submit(&main, &repo, "heavy", None).await;
    let head = queued(&main, &heavy).await;
    assert_eq!(
        head.position, 1,
        "the heavy order heads the queue: {head:?}"
    );

    let (small_client, small) = submit(&main, &repo, "small", None).await;
    let filled = until_exit(small_client, &small, ENDS).await;
    assert_eq!(
        filled.state,
        JobState::Passed,
        "the small order ends before the reserved start of the head: {filled:?}"
    );
    assert!(
        filled.output.contains("small-filled-the-hole"),
        "{}",
        filled.output
    );
    let still = queued(&main, &heavy).await;
    assert_eq!(still.position, 1, "{still:?}");

    let (long_client, long) = submit(&main, &repo, "long", None).await;
    let waits = queued(&main, &long).await;
    assert_eq!(
        waits.position, 2,
        "an order that would outlive the reservation waits behind the head: {waits:?}"
    );
    assert!(waits.wait_reason.is_some(), "{waits:?}");
    let view = status(&main).await.expect("status");
    assert_eq!(view.pool.used_mem_mb, 512, "{view:?}");

    open(&gate);
    let first = until_exit(running_client, &running, ENDS).await;
    assert_eq!(first.state, JobState::Passed, "{first:?}");
    let h = until_exit(heavy_client, &heavy, ENDS).await;
    assert_eq!(h.state, JobState::Passed, "{h:?}");
    let l = until_exit(long_client, &long, ENDS).await;
    assert_eq!(l.state, JobState::Passed, "{l:?}");
    let rows = history(&main).await;
    let started_at = |job| {
        rows.iter()
            .find(|r| &r.job == job)
            .and_then(|r| r.started_at)
            .unwrap_or_else(|| panic!("{job} started: {rows:?}"))
    };
    assert!(
        started_at(&heavy) <= started_at(&long),
        "the head starts first once the running job ends early: {rows:?}"
    );
    assert!(started_at(&small) < started_at(&heavy), "{rows:?}");

    roles.stop();
    drop(release);
    remove(dir).await;
}
