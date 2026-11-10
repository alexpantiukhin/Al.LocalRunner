mod common;

use common::harness::*;
use lr_core::{JobId, JobState};
use lr_proto::*;
use std::time::Duration;

const ALEX: &str = "local:alex";

async fn assigned(h: &Harness) -> (Conn, Conn) {
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 3600))).await;
    hub.accepted().await;
    assert_eq!(r.assign().await.job, JobId("j1".into()));
    (r, hub)
}

async fn cancel_ack(c: &mut Conn) -> CancelAck {
    loop {
        if let BrokerToHub::CancelAck(a) = c.hub().await {
            return a;
        }
    }
}

async fn runner_cancel(r: &mut Conn) -> RunnerCancel {
    loop {
        if let BrokerToRunner::Cancel(c) = r.runner().await {
            return c;
        }
    }
}

async fn rejoin(h: &Harness, name: &str) -> Conn {
    let mut c = h.dial(member_peer(&format!("pk-{name}"), "10.0.0.7")).await;
    c.send(&hello(name, Some(linux()))).await;
    c.welcome().await;
    c
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

async fn closed_without_new_assign(h: &Harness, reason: ReasonCode) {
    let h = h.restart().await;
    let row = h.until_state(ALEX, "j1", JobState::Cancelled).await;
    assert_eq!(
        row.job
            .cancel_reason
            .as_deref()
            .map(|r| r.split(':').next().unwrap()),
        Some(reason.to_string()).as_deref()
    );
    let mut back = rejoin(&h, "r1").await;
    back.quiet().await;
    let r = final_result(&h).await;
    assert_eq!((r.state, r.reason), (JobState::Cancelled, Some(reason)));
}

#[tokio::test]
async fn own_cancel_of_an_assigned_order_survives_a_restart() {
    let h = Harness::start("");
    let (mut r, mut hub) = assigned(&h).await;
    hub.send(&cancel("j1", 2)).await;
    assert_eq!(cancel_ack(&mut hub).await.outcome, CancelOutcome::Applied);
    runner_cancel(&mut r).await;
    closed_without_new_assign(&h, ReasonCode::CancelledByUser).await;
}

#[tokio::test]
async fn owner_cancel_of_an_assigned_order_survives_a_restart() {
    let h = Harness::start("[cancel]\nothers = \"owner\"\n");
    let (mut r, _hub) = assigned(&h).await;
    let mut root = h.local_hub("root").await;
    let HubToBroker::Cancel(mut c) = cancel("j1", 1) else {
        unreachable!()
    };
    c.client = Some(ALEX.into());
    root.send(&HubToBroker::Cancel(c)).await;
    assert_eq!(cancel_ack(&mut root).await.outcome, CancelOutcome::Applied);
    runner_cancel(&mut r).await;
    closed_without_new_assign(&h, ReasonCode::CancelledByUser).await;
}

#[tokio::test]
async fn hub_absent_cancel_of_an_assigned_order_survives_a_restart() {
    let h = Harness::start("[orphans]\nabsent_after_min = 1\nrunning = \"cancel\"\n");
    let (mut r, hub) = assigned(&h).await;
    hub.close().await;
    for _ in 0..3 {
        r.send(&RunnerToBroker::Heartbeat).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        h.clock.advance(Duration::from_secs(20));
    }
    h.poke().await;
    assert_eq!(runner_cancel(&mut r).await.code, ReasonCode::HubAbsent);
    let row = h.job_row(ALEX, "j1").await.unwrap().job;
    assert_eq!(
        (row.state, row.runner.as_deref()),
        (JobState::Queued, Some("r1"))
    );
    closed_without_new_assign(&h, ReasonCode::HubAbsent).await;
}

#[tokio::test]
async fn hub_absence_after_a_restart_is_counted_from_the_boot() {
    let h = Harness::start("[orphans]\nabsent_after_min = 10\n");
    let r = h.member_runner("r1", "10.0.0.7", linux()).await;
    r.close().await;
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("r1", false).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    hub.close().await;
    h.clock.advance(Duration::from_secs(300));
    let h = h.restart().await;
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
}

#[tokio::test]
async fn cancel_of_a_cancelling_job_is_sent_again_after_a_restart() {
    let h = Harness::start("");
    let (mut r, mut hub) = assigned(&h).await;
    r.send(&started("j1", 1)).await;
    hub.started_notice().await;
    hub.send(&cancel("j1", 2)).await;
    runner_cancel(&mut r).await;
    h.until_state(ALEX, "j1", JobState::Cancelling).await;
    let h = h.restart().await;
    let mut back = rejoin(&h, "r1").await;
    let c = runner_cancel(&mut back).await;
    assert_eq!(
        (c.job, c.code),
        (JobId("j1".into()), ReasonCode::CancelledByUser)
    );
}
