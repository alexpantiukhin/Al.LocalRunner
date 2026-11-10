mod common;

use common::faulty::FaultyStore;
use common::harness::*;
use lr_abstractions::Level;
use lr_core::{JobState, Timestamp};
use lr_proto::*;
use std::time::Duration;

const ALEX: &str = "local:alex";
const GRACE: &str = "[grace]\nlost_min = 2\n";
const DEADLINE: Duration = Duration::from_secs(60 + 120);
const SECOND: Duration = Duration::from_secs(1);

fn errors(h: &Harness) -> usize {
    h.log
        .records()
        .iter()
        .filter(|r| r.level == Level::Error)
        .count()
}

async fn assigned(h: &Harness) -> (Conn, Conn) {
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    (r, hub)
}

async fn started_while_store_fails(h: &Harness) -> (Conn, Conn) {
    let (mut r, hub) = assigned(h).await;
    FaultyStore::set(&h.faults.fail_transition);
    r.send(&started("j1", 1)).await;
    r.acked(1).await;
    (r, hub)
}

async fn store_back(h: &Harness) {
    FaultyStore::clear(&h.faults.fail_transition);
    h.clock.advance(SECOND);
    h.poke().await;
}

#[tokio::test]
async fn repeated_started_with_new_seqs_changes_nothing() {
    let h = Harness::start(GRACE);
    let (mut r, mut hub) = assigned(&h).await;
    r.send(&started("j1", 1)).await;
    r.acked(1).await;
    hub.started_notice().await;
    h.clock.advance(Duration::from_secs(10));
    r.send(&started("j1", 2)).await;
    r.acked(2).await;
    r.send(&started("j1", 3)).await;
    r.acked(3).await;
    let msgs = hub.sync().await;
    assert!(!msgs.iter().any(|m| matches!(m, BrokerToHub::Started(_))));
    let row = h.job_row(ALEX, "j1").await.unwrap();
    assert_eq!(row.job.started_at, Some(Timestamp(START)));
    h.clock.advance(SECOND);
    h.poke().await;
    assert_eq!(errors(&h), 0, "{:?}", h.log.records());
    h.clock.advance(DEADLINE - Duration::from_secs(11));
    h.poke().await;
    let lost = h.until_state(ALEX, "j1", JobState::Lost).await;
    assert_eq!(lost.job.finished_at, Some(Timestamp(START).plus_secs(180)));
}

#[tokio::test]
async fn store_outage_spanning_the_lost_deadline_writes_start_then_lost() {
    let h = Harness::start(GRACE);
    let _conns = started_while_store_fails(&h).await;
    h.clock.advance(DEADLINE);
    h.poke().await;
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Queued
    );
    store_back(&h).await;
    let row = h.until_state(ALEX, "j1", JobState::Lost).await;
    assert_eq!(row.job.started_at, Some(Timestamp(START)));
}

#[tokio::test]
async fn result_during_a_start_outage_is_written_after_the_start() {
    let h = Harness::start("");
    let (mut r, _hub) = started_while_store_fails(&h).await;
    r.send(&result("j1", 2, JobState::Passed, 0)).await;
    r.acked(2).await;
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Queued
    );
    store_back(&h).await;
    let row = h.until_state(ALEX, "j1", JobState::Passed).await;
    assert_eq!(row.job.started_at, Some(Timestamp(START)));
}

#[tokio::test]
async fn cancel_while_the_start_is_unsaved_ends_cancelled_with_its_canceller() {
    let h = Harness::start("");
    let (mut r, hub) = started_while_store_fails(&h).await;
    hub.send(&cancel("j1", 2)).await;
    loop {
        if let BrokerToRunner::Cancel(_) = r.runner().await {
            break;
        }
    }
    store_back(&h).await;
    h.until_state(ALEX, "j1", JobState::Cancelling).await;
    r.send(&result("j1", 2, JobState::Cancelled, 1)).await;
    r.acked(2).await;
    let row = h.until_state(ALEX, "j1", JobState::Cancelled).await;
    assert_eq!(row.job.started_at, Some(Timestamp(START)));
    assert_eq!(row.job.cancelled_by.as_deref(), Some("agent-1"));
    assert_eq!(
        row.job.cancel_reason.as_deref(),
        Some("cancelled_by_user: stop")
    );
}

#[tokio::test]
async fn a_runner_gone_while_the_start_is_unsaved_keeps_the_run_active() {
    let h = Harness::start("");
    let (r, mut hub) = started_while_store_fails(&h).await;
    r.close().await;
    hub.until_runner("r1", false).await;
    store_back(&h).await;
    let row = h.until_state(ALEX, "j1", JobState::Running).await;
    assert_eq!(row.job.started_at, Some(Timestamp(START)));
    let mut r2 = h.member_runner("r2", "10.0.0.8", linux()).await;
    r2.quiet().await;
}
