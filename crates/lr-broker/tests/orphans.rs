mod common;

use common::harness::*;
use lr_core::{JobId, JobState};
use lr_proto::*;
use std::time::Duration;

const ALEX: &str = "local:alex";

async fn waiting_order(h: &Harness) {
    let r = h.member_runner("r1", "10.0.0.7", linux()).await;
    r.close().await;
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("r1", false).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    hub.close().await;
}

#[tokio::test]
async fn hub_absent_cancels_waiting_orders_after_absent_after_min() {
    let h = Harness::start("[orphans]\nabsent_after_min = 10\n");
    waiting_order(&h).await;
    h.clock.advance(Duration::from_millis(600_000 - 1));
    h.poke().await;
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Queued
    );
    h.clock.advance(Duration::from_millis(1));
    h.poke().await;
    let row = h.until_state(ALEX, "j1", JobState::Cancelled).await;
    assert_eq!(row.job.cancel_reason.as_deref(), Some("hub_absent"));
    let mut hub = h.local_hub("alex").await;
    hub.send(&HubToBroker::Resume(Resume { points: Vec::new() }))
        .await;
    let r = loop {
        if let BrokerToHub::JobResult(r) = hub.hub().await {
            break r;
        }
    };
    assert_eq!(
        (r.state, r.reason),
        (JobState::Cancelled, Some(ReasonCode::HubAbsent))
    );
}

#[tokio::test]
async fn waiting_orders_are_kept_with_queued_keep() {
    let h = Harness::start("[orphans]\nabsent_after_min = 10\nqueued = \"keep\"\n");
    waiting_order(&h).await;
    h.clock.advance(Duration::from_secs(3600));
    h.poke().await;
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Queued
    );
}

#[tokio::test]
async fn a_returning_hub_resets_the_absence() {
    let h = Harness::start("[orphans]\nabsent_after_min = 10\n");
    waiting_order(&h).await;
    h.clock.advance(Duration::from_secs(300));
    let hub = h.local_hub("alex").await;
    hub.close().await;
    h.clock.advance(Duration::from_secs(400));
    h.poke().await;
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Queued
    );
}

async fn running_with_absent_hub(h: &Harness) -> Conn {
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 3600))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    r.acked(1).await;
    hub.started_notice().await;
    hub.close().await;
    for _ in 0..2 {
        h.clock.advance(Duration::from_secs(30));
        r.send(&RunnerToBroker::Heartbeat).await;
        h.poke().await;
    }
    r
}

#[tokio::test]
async fn running_job_is_cancelled_when_orphans_running_is_cancel() {
    let h = Harness::start("[orphans]\nabsent_after_min = 1\nrunning = \"cancel\"\n");
    let mut r = running_with_absent_hub(&h).await;
    let c = loop {
        if let BrokerToRunner::Cancel(c) = r.runner().await {
            break c;
        }
    };
    assert_eq!((c.job, c.code), (JobId("j1".into()), ReasonCode::HubAbsent));
    h.until_state(ALEX, "j1", JobState::Cancelling).await;
}

#[tokio::test]
async fn running_job_finishes_by_default_when_the_hub_is_absent() {
    let h = Harness::start("[orphans]\nabsent_after_min = 1\n");
    let mut r = running_with_absent_hub(&h).await;
    r.quiet().await;
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Running
    );
}
