mod common;

use common::harness::*;
use lr_core::{JobId, JobState};
use lr_proto::*;
use std::time::Duration;

const ALEX: &str = "local:alex";
const NO_RUNNER: &str = "rejected_no_runner_on_machine";

async fn returned(hub: &mut Conn) -> Returned {
    loop {
        if let BrokerToHub::Returned(r) = hub.hub().await {
            return r;
        }
    }
}

#[tokio::test]
async fn same_machine_waits_only_if_runner_online_but_busy() {
    let h = Harness::start("");
    let mut r = h.local_runner("alex", "lr", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("s1", same_machine_req())).await;
    assert_eq!(hub.accepted().await.waiting, None);
    assert_eq!(r.assign().await.job, JobId("s1".into()));
    hub.send(&order("s2", same_machine_req())).await;
    assert_eq!(
        hub.accepted().await.waiting.as_deref(),
        Some("раннер занят")
    );
    assert_eq!(
        h.job_row(ALEX, "s2").await.unwrap().job.state,
        JobState::Queued
    );
}

#[tokio::test]
async fn same_machine_rejected_if_runner_offline_or_gone_before_start() {
    let h = Harness::start("");
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("none", same_machine_req())).await;
    assert_eq!(hub.rejected().await.code.as_str(), NO_RUNNER);

    let r = h.local_runner("alex", "lr", linux()).await;
    r.close().await;
    hub.until_runner("local:lr", false).await;
    hub.send(&order("offline", same_machine_req())).await;
    assert_eq!(hub.rejected().await.code.as_str(), NO_RUNNER);
    assert!(h.job_row(ALEX, "offline").await.is_none());

    let mut r = h.local_runner("alex", "lr", linux()).await;
    hub.send(&order("s1", same_machine_req())).await;
    hub.accepted().await;
    r.assign().await;
    hub.send(&order("s2", same_machine_req())).await;
    hub.accepted().await;
    r.close().await;
    let mut got = [returned(&mut hub).await, returned(&mut hub).await];
    got.sort_by(|a, b| a.job.0.cmp(&b.job.0));
    assert_eq!(got[0].job, JobId("s1".into()));
    assert_eq!(got[1].job, JobId("s2".into()));
    assert!(got
        .iter()
        .all(|r| r.code.as_str() == NO_RUNNER && r.seq == 1));
    for job in ["s1", "s2"] {
        let row = h.until_state(ALEX, job, JobState::Cancelled).await;
        assert!(row.job.cancel_reason.unwrap().starts_with(NO_RUNNER));
    }
}

#[tokio::test]
async fn same_machine_order_keeps_waiting_while_another_runner_of_the_installation_lives() {
    let h = Harness::start("");
    let mut busy = h.local_runner("alex", "lr2", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("s0", same_machine_req())).await;
    hub.accepted().await;
    busy.assign().await;
    busy.send(&started("s0", 1)).await;
    hub.started_notice().await;
    let mut r1 = h.local_runner("alex", "lr1", linux()).await;
    hub.send(&order("s1", same_machine_req())).await;
    hub.accepted().await;
    r1.assign().await;
    r1.close().await;
    hub.until_runner("local:lr1", false).await;
    let row = h.job_row(ALEX, "s1").await.unwrap();
    assert_eq!((row.job.state, row.job.runner), (JobState::Queued, None));
    let msgs = hub.sync().await;
    assert!(!msgs.iter().any(|m| matches!(m, BrokerToHub::Returned(_))));
}

#[tokio::test]
async fn draining_runner_does_not_count_for_a_same_machine_order() {
    let h = Harness::start("");
    let r = h.local_runner("alex", "lr", linux()).await;
    r.send(&RunnerToBroker::Drain).await;
    let mut hub = h.local_hub("alex").await;
    hub.sync().await;
    hub.send(&order("s1", same_machine_req())).await;
    assert_eq!(hub.rejected().await.code.as_str(), NO_RUNNER);
}

#[tokio::test]
async fn queued_same_machine_order_is_returned_when_its_last_runner_drains() {
    let h = Harness::start("");
    let mut r = h.local_runner("alex", "lr", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("s1", same_machine_req())).await;
    hub.accepted().await;
    r.assign().await;
    hub.send(&order("s2", same_machine_req())).await;
    hub.accepted().await;
    r.send(&RunnerToBroker::Drain).await;
    let back = returned(&mut hub).await;
    assert_eq!(
        (back.job, back.code.as_str()),
        (JobId("s2".into()), NO_RUNNER)
    );
    h.until_state(ALEX, "s2", JobState::Cancelled).await;
}

#[tokio::test]
async fn restored_same_machine_order_waits_one_grace_period_for_its_runner() {
    let h = Harness::start("[grace]\nlost_min = 2\n");
    let mut r = h.local_runner("alex", "lr", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("s1", same_machine_req())).await;
    hub.accepted().await;
    r.assign().await;
    let h = h.restart().await;
    h.clock.advance(Duration::from_millis(120_000 - 1));
    h.poke().await;
    assert_eq!(
        h.job_row(ALEX, "s1").await.unwrap().job.state,
        JobState::Queued
    );
    h.clock.advance(Duration::from_millis(1));
    h.poke().await;
    h.until_state(ALEX, "s1", JobState::Cancelled).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&HubToBroker::Resume(Resume { points: Vec::new() }))
        .await;
    assert_eq!(returned(&mut hub).await.code.as_str(), NO_RUNNER);
}

#[tokio::test]
async fn restored_same_machine_order_runs_when_its_runner_comes_back() {
    let h = Harness::start("[grace]\nlost_min = 2\n");
    let mut r = h.local_runner("alex", "lr", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("s1", same_machine_req())).await;
    hub.accepted().await;
    r.assign().await;
    let h = h.restart().await;
    let mut back = h.local_runner("alex", "lr", linux()).await;
    assert_eq!(back.assign().await.job, JobId("s1".into()));
}
