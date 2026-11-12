mod common;

use common::faulty::FaultyStore;
use common::harness::*;
use lr_abstractions::Level;
use lr_core::{JobId, JobState};
use lr_proto::*;
use std::time::Duration;

const ALEX: &str = "local:alex";
const SECOND: Duration = Duration::from_secs(1);

fn logged_error(h: &Harness) -> bool {
    h.log.records().iter().any(|r| r.level == Level::Error)
}

async fn retried(h: &Harness, flag: &std::sync::atomic::AtomicBool) {
    assert!(logged_error(h), "{:?}", h.log.records());
    FaultyStore::clear(flag);
    h.clock.advance(SECOND);
    h.poke().await;
}

async fn cancel_ack(c: &mut Conn) -> CancelAck {
    loop {
        if let BrokerToHub::CancelAck(a) = c.hub().await {
            return a;
        }
    }
}

async fn waiting_order(h: &Harness) -> Conn {
    let r = h.member_runner("r1", "10.0.0.7", linux()).await;
    r.close().await;
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("r1", false).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    hub
}

#[tokio::test]
async fn failed_lost_write_is_retried_on_the_next_tick() {
    let h = Harness::start("[grace]\nlost_min = 2\n");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    hub.started_notice().await;
    r.close().await;
    hub.until_runner("r1", false).await;
    FaultyStore::set(&h.faults.fail_transition);
    h.clock.advance(Duration::from_secs(180));
    h.poke().await;
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Running
    );
    retried(&h, &h.faults.fail_transition).await;
    h.until_state(ALEX, "j1", JobState::Lost).await;
}

#[tokio::test]
async fn failed_requeue_at_start_is_retried_on_the_next_tick() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    let h = h.restart_with(|f| FaultyStore::set(&f.fail_requeue)).await;
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.runner.as_deref(),
        Some("r1")
    );
    retried(&h, &h.faults.fail_requeue).await;
    let row = h.job_row(ALEX, "j1").await.unwrap();
    assert_eq!((row.job.state, row.job.runner), (JobState::Queued, None));
    let mut back = h.dial(member_peer("pk-r1", "10.0.0.7")).await;
    back.send(&hello("r1", Some(linux()))).await;
    back.welcome().await;
    assert_eq!(back.assign().await.job, JobId("j1".into()));
}

#[tokio::test]
async fn failed_requeue_of_a_vanished_runner_is_retried_on_the_next_tick() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    FaultyStore::set(&h.faults.fail_requeue);
    r.close().await;
    hub.until_runner("r1", false).await;
    retried(&h, &h.faults.fail_requeue).await;
    let mut r2 = h.member_runner("r2", "10.0.0.8", linux()).await;
    assert_eq!(r2.assign().await.job, JobId("j1".into()));
}

#[tokio::test]
async fn failed_cancel_of_a_waiting_order_is_retried_on_the_next_tick() {
    let h = Harness::start("");
    let mut hub = waiting_order(&h).await;
    FaultyStore::set(&h.faults.fail_transition);
    hub.send(&cancel("j1", 2)).await;
    assert_eq!(cancel_ack(&mut hub).await.outcome, CancelOutcome::Applied);
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Queued
    );
    retried(&h, &h.faults.fail_transition).await;
    h.until_state(ALEX, "j1", JobState::Cancelled).await;
}

#[tokio::test]
async fn failed_orphan_cancel_is_retried_on_the_next_tick() {
    let h = Harness::start("[orphans]\nabsent_after_min = 10\n");
    let hub = waiting_order(&h).await;
    hub.close().await;
    FaultyStore::set(&h.faults.fail_transition);
    h.clock.advance(Duration::from_secs(600));
    h.poke().await;
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Queued
    );
    retried(&h, &h.faults.fail_transition).await;
    let row = h.until_state(ALEX, "j1", JobState::Cancelled).await;
    assert_eq!(row.job.cancel_reason.as_deref(), Some("hub_absent"));
}

async fn assigned_on_r1(h: &Harness, job: &str) -> (Conn, Conn) {
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order(job, req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    (r, hub)
}

async fn rejoin_r1(h: &Harness) -> Conn {
    let mut back = h.dial(member_peer("pk-r1", "10.0.0.7")).await;
    back.send(&hello("r1", Some(linux()))).await;
    back.welcome().await;
    back
}

#[tokio::test]
async fn cancel_of_a_run_parked_by_a_failed_requeue_at_start_is_kept() {
    let h = Harness::start("");
    assigned_on_r1(&h, "j1").await;
    let h = h.restart_with(|f| FaultyStore::set(&f.fail_requeue)).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&cancel("j1", 2)).await;
    assert_eq!(cancel_ack(&mut hub).await.outcome, CancelOutcome::Applied);
    retried(&h, &h.faults.fail_requeue).await;
    h.until_state(ALEX, "j1", JobState::Cancelled).await;
    rejoin_r1(&h).await.quiet().await;
}

#[tokio::test]
async fn cancel_of_a_run_parked_after_its_runner_vanished_is_kept() {
    let h = Harness::start("");
    let (r, mut hub) = assigned_on_r1(&h, "j1").await;
    FaultyStore::set(&h.faults.fail_requeue);
    r.close().await;
    hub.until_runner("r1", false).await;
    hub.send(&cancel("j1", 2)).await;
    assert_eq!(cancel_ack(&mut hub).await.outcome, CancelOutcome::Applied);
    retried(&h, &h.faults.fail_requeue).await;
    h.until_state(ALEX, "j1", JobState::Cancelled).await;
    let mut r2 = h.member_runner("r2", "10.0.0.8", linux()).await;
    r2.quiet().await;
}

#[tokio::test]
async fn a_failing_retry_round_logs_one_line_with_the_count() {
    let h = Harness::start("");
    let (r, mut hub) = assigned_on_r1(&h, "j1").await;
    let mut r2 = h.member_runner("r2", "10.0.0.8", linux()).await;
    hub.send(&order("j2", req(512, 1, 60))).await;
    hub.accepted().await;
    r2.assign().await;
    FaultyStore::set(&h.faults.fail_requeue);
    r.close().await;
    r2.close().await;
    hub.until_runner("r1", false).await;
    hub.until_runner("r2", false).await;
    let errors = |h: &Harness| -> Vec<String> {
        h.log
            .records()
            .into_iter()
            .filter(|r| r.level == Level::Error)
            .map(|r| r.message)
            .collect()
    };
    assert_eq!(errors(&h).len(), 2);
    h.clock.advance(SECOND);
    h.poke().await;
    let after = errors(&h);
    assert_eq!(after.len(), 3, "{after:?}");
    assert_eq!(
        after[2],
        "storage failed: 2 writes still failing; retried in a second"
    );
}

#[tokio::test]
async fn failed_started_write_is_retried_on_the_next_tick() {
    let h = Harness::start("");
    let (mut r, _hub) = assigned_on_r1(&h, "j1").await;
    FaultyStore::set(&h.faults.fail_transition);
    r.send(&started("j1", 1)).await;
    r.acked(1).await;
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Queued
    );
    retried(&h, &h.faults.fail_transition).await;
    let row = h.until_state(ALEX, "j1", JobState::Running).await;
    assert_eq!(row.job.started_at, Some(lr_core::Timestamp(START)));
}

#[tokio::test]
async fn result_after_a_failed_started_write_still_ends_the_run() {
    let h = Harness::start("");
    let (mut r, _hub) = assigned_on_r1(&h, "j1").await;
    FaultyStore::set(&h.faults.fail_transition);
    r.send(&started("j1", 1)).await;
    r.acked(1).await;
    FaultyStore::clear(&h.faults.fail_transition);
    r.send(&result("j1", 2, JobState::Passed, 0)).await;
    r.acked(2).await;
    h.until_state(ALEX, "j1", JobState::Passed).await;
}

#[tokio::test]
async fn a_runner_message_whose_seq_was_not_stored_closes_the_connection_and_is_sent_again() {
    let h = Harness::start("");
    let (mut r, _hub) = assigned_on_r1(&h, "j1").await;
    r.send(&started("j1", 1)).await;
    r.acked(1).await;
    FaultyStore::set(&h.faults.fail_accept_seq);
    r.send(&result("j1", 2, JobState::Passed, 0)).await;
    assert!(r.closed().await, "the runner must reconnect and resend");
    assert!(logged_error(&h), "{:?}", h.log.records());
    FaultyStore::clear(&h.faults.fail_accept_seq);
    let mut back = rejoin_r1(&h).await;
    back.send(&result("j1", 2, JobState::Passed, 0)).await;
    back.acked(2).await;
    h.until_state(ALEX, "j1", JobState::Passed).await;
}

#[tokio::test]
async fn a_hub_message_not_kept_in_the_outbox_is_retried() {
    let h = Harness::start("");
    let (mut r, mut hub) = assigned_on_r1(&h, "j1").await;
    r.send(&started("j1", 1)).await;
    r.acked(1).await;
    hub.started_notice().await;
    FaultyStore::set(&h.faults.fail_push_result);
    r.send(&result("j1", 2, JobState::Passed, 0)).await;
    r.acked(2).await;
    h.until_state(ALEX, "j1", JobState::Passed).await;
    retried(&h, &h.faults.fail_push_result).await;
    loop {
        if let BrokerToHub::JobResult(res) = hub.hub().await {
            assert_eq!(res.state, JobState::Passed);
            break;
        }
    }
}

#[tokio::test]
async fn a_repeated_order_of_a_run_parked_by_a_failed_requeue_makes_no_second_run() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    FaultyStore::set(&h.faults.fail_requeue);
    r.close().await;
    hub.until_runner("r1", false).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    retried(&h, &h.faults.fail_requeue).await;
    match hub.read(ReadRequest::Status(StatusFilter::default())).await {
        ReadReply::Status(s) => assert_eq!(s.queue.len(), 1, "{:?}", s.queue),
        other => panic!("expected status, got {other:?}"),
    }
}
