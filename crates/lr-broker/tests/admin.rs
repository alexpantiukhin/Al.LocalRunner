mod common;

use common::harness::*;
use lr_core::{JobId, JobState};
use lr_proto::*;

#[tokio::test]
async fn reload_with_new_grace_sends_settings_update_to_runners() {
    let h = Harness::start("");
    let mut runner = h.local_runner("alex", "r1", linux()).await;
    let text = policy("[grace]\nlost_min = 7\n");
    let applied = h.handle.reload_policy(&text).await.expect("reload applies");
    assert_eq!(
        applied.settings_update.and_then(|u| u.broker_lost_kill_s),
        Some(420)
    );
    loop {
        match runner.runner().await {
            BrokerToRunner::SettingsUpdate(u) => {
                assert_eq!(u.broker_lost_kill_s, Some(420));
                break;
            }
            BrokerToRunner::Ack(_) => continue,
            other => panic!("expected settings_update, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn invalid_reload_is_refused_and_nothing_is_sent() {
    let h = Harness::start("");
    let mut runner = h.local_runner("alex", "r1", linux()).await;
    let err = h
        .handle
        .reload_policy("[grace]\nlost_min = 0\n")
        .await
        .unwrap_err();
    assert_eq!(err.key.as_deref(), Some("lost_min"));
    runner.quiet().await;
}

#[tokio::test]
async fn revoked_machine_loses_its_live_connection() {
    let h = Harness::start("");
    let mut runner = h.member_runner("box", "10.0.0.5", linux()).await;
    h.handle.revoked("box").await;
    assert!(runner.closed().await, "the revoked runner stays connected");
}

#[tokio::test]
async fn removed_runner_loses_its_running_job_and_waiting_orders_stay() {
    let h = Harness::start("");
    let mut r = h.member_runner("box", "10.0.0.5", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 3600))).await;
    hub.accepted().await;
    let a = r.assign().await;
    assert_eq!(a.job, JobId("j1".into()));
    r.send(&started("j1", 1)).await;
    r.acked(1).await;
    hub.started_notice().await;
    hub.send(&order("j2", req(512, 1, 3600))).await;
    hub.accepted().await;

    h.handle.revoked("box").await;

    let result = loop {
        if let BrokerToHub::JobResult(res) = hub.hub().await {
            break res;
        }
    };
    assert_eq!(result.job, JobId("j1".into()));
    assert_eq!(result.state, JobState::Lost);
    assert_eq!(result.reason, Some(ReasonCode::RunnerRemoved));
    let row = h.job_row("local:alex", "j1").await.unwrap();
    assert_eq!(row.job.state, JobState::Lost);
    assert_eq!(
        h.job_row("local:alex", "j2").await.unwrap().job.state,
        JobState::Queued
    );

    let mut other = h.member_runner("spare", "10.0.0.6", linux()).await;
    assert_eq!(other.assign().await.job, JobId("j2".into()));
}
