mod common;

use common::faulty::FaultyStore;
use common::harness::*;
use lr_abstractions::{BrokerStore, JobKey};
use lr_broker::RETENTION_EVERY_S;
use lr_core::JobState;
use std::sync::atomic::Ordering;
use std::time::Duration;

const ALEX: &str = "local:alex";
const MONTH_S: u64 = 30 * 86_400;
const RETENTION_EVERY: Duration = Duration::from_secs(RETENTION_EVERY_S);

async fn eventually<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..2000 {
        if f().await {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("never happened: {what}");
}

async fn settle() {
    for _ in 0..2000 {
        tokio::task::yield_now().await;
    }
}

async fn log_len(h: &Harness, job: &str) -> usize {
    h.store
        .read_log(&JobKey::new(ALEX, job))
        .await
        .unwrap()
        .len()
}

async fn pending(h: &Harness) -> usize {
    h.store.pending_results(ALEX).await.unwrap().len()
}

#[tokio::test]
async fn history_retention_prunes_on_the_tick() {
    let h = Harness::start("[history]\nkeep_days = 1\nrows_keep_days = 2\n");
    let mut r = h.member_runner("r1", "10.0.0.5", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, MONTH_S))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    r.send(&chunk("j1", 2, "output\n")).await;
    r.send(&result("j1", 3, JobState::Passed, 0)).await;
    r.acked(3).await;
    hub.send(&order("j2", req(512, 1, MONTH_S))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j2", 1)).await;
    r.acked(1).await;
    h.until_state(ALEX, "j1", JobState::Passed).await;
    assert!(log_len(&h, "j1").await > 0);

    h.clock.advance(Duration::from_secs(86_399));
    h.poke().await;
    settle().await;
    assert!(
        log_len(&h, "j1").await > 0,
        "the log is younger than keep_days"
    );

    h.clock.advance(RETENTION_EVERY);
    h.poke().await;
    eventually("the log of j1 is pruned", || async {
        log_len(&h, "j1").await == 0
    })
    .await;
    assert!(h.job_row(ALEX, "j1").await.is_some(), "rows live longer");

    h.clock.advance(Duration::from_secs(86_400));
    h.poke().await;
    eventually("the row of j1 is pruned", || async {
        h.job_row(ALEX, "j1").await.is_none()
    })
    .await;
    assert_eq!(
        h.job_row(ALEX, "j2").await.unwrap().job.state,
        JobState::Running,
        "an active run is never pruned"
    );
}

#[tokio::test]
async fn results_expire_after_keep_hours() {
    let h = Harness::start("[results]\nkeep_hours = 1\n");
    expires_after_keep_hours(&h).await;
}

#[tokio::test]
async fn a_panicking_pass_does_not_stop_retention() {
    let h = Harness::start("[results]\nkeep_hours = 1\n");
    FaultyStore::set(&h.faults.panic_prune);
    expires_after_keep_hours(&h).await;
    assert!(
        !h.faults.panic_prune.load(Ordering::SeqCst),
        "a pass panicked"
    );
}

async fn expires_after_keep_hours(h: &Harness) {
    let r = h.member_runner("r0", "10.0.0.7", linux()).await;
    r.close().await;
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("r0", false).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    hub.send(&cancel("j1", 2)).await;
    hub.sync().await;
    hub.close().await;
    assert!(pending(h).await > 0);

    h.clock.advance(Duration::from_secs(3_599));
    h.poke().await;
    settle().await;
    assert!(pending(h).await > 0, "kept for keep_hours");

    h.clock.advance(RETENTION_EVERY);
    h.poke().await;
    eventually("results for the absent hub are removed", || async {
        pending(h).await == 0
    })
    .await;
}
