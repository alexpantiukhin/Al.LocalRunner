mod common;

use common::harness::*;
use lr_core::{JobId, JobState, Timestamp};
use std::time::Duration;

const ALEX: &str = "local:alex";
const GRACE: &str = "[grace]\nlost_min = 2\n";

#[tokio::test]
async fn the_queue_time_is_the_brokers_own_whatever_the_hub_claims() {
    let h = Harness::start("");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    h.clock.advance(Duration::from_secs(5));
    let mut hub = h.local_hub("alex").await;
    hub.send(&order_at("j1", req(512, 1, 60), 0)).await;
    hub.accepted().await;
    let row = h.job_row(ALEX, "j1").await.unwrap();
    assert_eq!(row.job.queued_at, Timestamp(START + 5_000));
}

#[tokio::test]
async fn a_claimed_ancient_queue_time_does_not_jump_the_queue() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j0", req(512, 1, 60))).await;
    hub.accepted().await;
    assert_eq!(r.assign().await.job, JobId("j0".into()));
    r.send(&started("j0", 1)).await;
    r.acked(1).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    h.clock.advance(Duration::from_secs(1));
    hub.send(&order_at("j2", req(512, 1, 60), 0)).await;
    hub.accepted().await;
    r.send(&result("j0", 2, JobState::Passed, 0)).await;
    assert_eq!(r.assign().await.job, JobId("j1".into()));
}

async fn started_then_broker_down(h: &Harness, down_s: u64) -> Harness {
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    hub.started_notice().await;
    r.close().await;
    h.clock.advance(Duration::from_secs(down_s));
    h.restart().await
}

#[tokio::test]
async fn after_a_restart_the_lost_timer_waits_for_the_runner_one_grace() {
    let h = Harness::start(GRACE);
    let h = started_then_broker_down(&h, 170).await;
    h.clock.advance(Duration::from_secs(10));
    h.poke().await;
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Running,
        "the runner gets a grace from the broker start to come back"
    );
    h.clock.advance(Duration::from_secs(30));
    let mut back = h.dial(member_peer("pk-r1", "10.0.0.7")).await;
    back.send(&hello("r1", Some(linux()))).await;
    back.welcome().await;
    back.send(&result("j1", 2, JobState::Passed, 0)).await;
    back.acked(2).await;
    h.until_state(ALEX, "j1", JobState::Passed).await;
}

#[tokio::test]
async fn after_a_restart_a_runner_that_never_returns_is_lost_one_grace_after_the_start() {
    let h = Harness::start(GRACE);
    let h = started_then_broker_down(&h, 170).await;
    h.clock.advance(Duration::from_millis(120_000 - 1));
    h.poke().await;
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Running
    );
    h.clock.advance(Duration::from_millis(1));
    h.poke().await;
    let row = h.until_state(ALEX, "j1", JobState::Lost).await;
    assert_eq!(row.job.finished_at, Some(Timestamp(START + 290_000)));
}
