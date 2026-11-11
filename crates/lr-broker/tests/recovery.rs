mod common;

use common::harness::*;
use lr_core::{JobId, JobState, Timestamp};
use lr_proto::*;
use std::time::Duration;

const ALEX: &str = "local:alex";
const GRACE: &str = "[grace]\nlost_min = 2\n";
const DEADLINE_MS: u64 = 60_000 + 120_000;

async fn rejoin(h: &Harness, name: &str) -> Conn {
    let mut c = h.dial(member_peer(&format!("pk-{name}"), "10.0.0.7")).await;
    c.send(&hello(name, Some(linux()))).await;
    c.welcome().await;
    c
}

async fn started_on_vanished_runner(h: &Harness) -> Conn {
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(16384, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    hub.started_notice().await;
    r.close().await;
    hub.until_runner("r1", false).await;
    hub
}

async fn final_result(h: &Harness) -> JobResult {
    let mut hub = h.local_hub("alex").await;
    hub.send(&HubToBroker::Resume(Resume { points: Vec::new() }))
        .await;
    loop {
        if let BrokerToHub::JobResult(r) = hub.hub().await {
            return r;
        }
    }
}

#[tokio::test]
async fn lost_fires_exactly_at_started_plus_timeout_plus_grace() {
    let h = Harness::start(GRACE);
    started_on_vanished_runner(&h).await;
    h.clock.advance(Duration::from_millis(DEADLINE_MS - 1));
    h.poke().await;
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Running
    );
    h.clock.advance(Duration::from_millis(1));
    h.poke().await;
    let row = h.until_state(ALEX, "j1", JobState::Lost).await;
    assert_eq!(row.job.finished_at, Some(Timestamp(START + DEADLINE_MS)));
    assert_eq!(row.job.cancel_reason.as_deref(), Some("runner_lost"));
    let r = final_result(&h).await;
    assert_eq!(
        (r.state, r.reason),
        (JobState::Lost, Some(ReasonCode::RunnerLost))
    );
}

#[tokio::test]
async fn lost_job_returns_its_resources_to_the_pool() {
    let h = Harness::start(GRACE);
    started_on_vanished_runner(&h).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j2", req(16384, 1, 60))).await;
    hub.accepted().await;
    h.clock.advance(Duration::from_millis(DEADLINE_MS - 1));
    let mut r2 = h.member_runner("r2", "10.0.0.8", linux()).await;
    r2.quiet().await;
    h.clock.advance(Duration::from_millis(1));
    h.poke().await;
    assert_eq!(r2.assign().await.job, JobId("j2".into()));
}

#[tokio::test]
async fn runner_report_before_deadline_cancels_lost() {
    let h = Harness::start(GRACE);
    started_on_vanished_runner(&h).await;
    h.clock.advance(Duration::from_millis(DEADLINE_MS - 1_000));
    let mut r = rejoin(&h, "r1").await;
    r.send(&result("j1", 2, JobState::Passed, 0)).await;
    r.acked(2).await;
    h.clock.advance(Duration::from_secs(2));
    h.poke().await;
    let row = h.job_row(ALEX, "j1").await.unwrap();
    assert_eq!(row.job.state, JobState::Passed);
    assert_eq!(
        row.job.finished_at,
        Some(Timestamp(START + DEADLINE_MS - 1_000))
    );
    assert_eq!(final_result(&h).await.state, JobState::Passed);
}

#[tokio::test]
async fn cancelling_job_on_vanished_runner_ends_lost() {
    let h = Harness::start(GRACE);
    let hub = started_on_vanished_runner(&h).await;
    hub.send(&cancel("j1", 2)).await;
    h.until_state(ALEX, "j1", JobState::Cancelling).await;
    h.clock.advance(Duration::from_millis(DEADLINE_MS));
    h.poke().await;
    h.until_state(ALEX, "j1", JobState::Lost).await;
}

#[tokio::test]
async fn restart_requeues_unstarted_and_marks_started_unknown_as_lost_by_deadline() {
    let h = Harness::start(GRACE);
    let mut r1 = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(8192, 1, 60))).await;
    hub.accepted().await;
    assert_eq!(r1.assign().await.job, JobId("j1".into()));
    r1.send(&started("j1", 1)).await;
    hub.started_notice().await;
    let mut r2 = h.member_runner("r2", "10.0.0.8", linux()).await;
    hub.send(&order_at("j2", req(8192, 1, 60), START + 1)).await;
    hub.accepted().await;
    assert_eq!(r2.assign().await.job, JobId("j2".into()));
    hub.send(&order_at("j3", req(512, 1, 60), START + 2)).await;
    hub.accepted().await;
    let j3_order = lr_abstractions::BrokerStore::orders(h.store.as_ref())
        .await
        .unwrap()
        .remove(0);
    assert_eq!(j3_order.key.job, JobId("j3".into()));

    let h = h.restart().await;
    let j2 = h.job_row(ALEX, "j2").await.unwrap();
    assert_eq!((j2.job.state, j2.job.runner), (JobState::Queued, None));
    let j3 = h.job_row(ALEX, "j3").await.unwrap();
    assert_eq!((j3.job.state, j3.job.runner), (JobState::Queued, None));
    let queue = lr_abstractions::BrokerStore::orders(h.store.as_ref())
        .await
        .unwrap();
    let names: Vec<&str> = queue.iter().map(|o| o.key.job.0.as_str()).collect();
    assert_eq!(names, ["j2", "j3"]);
    assert_eq!(queue[1], j3_order);
    let mut hub = h.local_hub("alex").await;
    hub.send(&order_at("j4", req(512, 1, 60), START + 3)).await;
    hub.accepted().await;

    h.clock.advance(Duration::from_millis(DEADLINE_MS - 1));
    h.poke().await;
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Running
    );
    h.clock.advance(Duration::from_millis(1));
    h.poke().await;
    let j1 = h.until_state(ALEX, "j1", JobState::Lost).await;
    assert_eq!(j1.job.finished_at, Some(Timestamp(START + DEADLINE_MS)));

    let mut back = rejoin(&h, "r2").await;
    let a = back.assign().await;
    assert_eq!((a.job, a.seq), (JobId("j2".into()), 1));
}

#[tokio::test]
async fn queued_order_from_before_a_restart_is_assigned_after_it() {
    let h = Harness::start("");
    let r = h.member_runner("r1", "10.0.0.7", linux()).await;
    r.close().await;
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("r1", false).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    let h = h.restart().await;
    let mut back = rejoin(&h, "r1").await;
    assert_eq!(back.assign().await.job, JobId("j1".into()));
}

#[tokio::test]
async fn re_hello_under_the_same_name_requeues_the_unstarted_assignment() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    assert_eq!(r.assign().await.seq, 1);
    let mut again = rejoin(&h, "r1").await;
    let a = again.assign().await;
    assert_eq!((a.job, a.seq), (JobId("j1".into()), 1));
    let row = h.job_row(ALEX, "j1").await.unwrap();
    assert_eq!(row.job.runner.as_deref(), Some("r1"));
}

#[tokio::test]
async fn drain_finishes_running_and_refuses_new() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    r.acked(1).await;
    r.send(&RunnerToBroker::Drain).await;
    hub.send(&order("j2", req(512, 1, 60))).await;
    let waiting = hub.accepted().await;
    assert_eq!(
        waiting.waiting.as_deref(),
        Some("подходящие раннеры offline")
    );
    r.send(&result("j1", 2, JobState::Passed, 0)).await;
    r.acked(2).await;
    r.quiet().await;
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Passed
    );
    assert_eq!(
        h.job_row(ALEX, "j2").await.unwrap().job.state,
        JobState::Queued
    );
    let rows = lr_abstractions::BrokerStore::runners(h.store.as_ref())
        .await
        .unwrap();
    assert_eq!(rows[0].state, lr_abstractions::RunnerState::Draining);
}

#[tokio::test]
async fn after_a_restart_runner_silence_counts_from_its_first_heartbeat() {
    let h = Harness::start(GRACE);
    let _before = h.member_runner("r1", "10.0.0.7", linux()).await;
    h.clock.advance(Duration::from_secs(40));
    let h = h.restart().await;
    let mut r = rejoin(&h, "r1").await;
    h.clock.advance(Duration::from_secs(10));
    r.send(&RunnerToBroker::Heartbeat).await;
    h.local_hub("alex").await.until_runner("r1", true).await;
    h.clock.advance(Duration::from_millis(45_000 - 1));
    h.local_hub("alex").await.until_runner("r1", true).await;
    h.clock.advance(Duration::from_millis(1));
    h.local_hub("alex").await.until_runner("r1", false).await;
    assert!(r.closed().await);
}
