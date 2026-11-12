mod common;

use common::harness::*;
use lr_abstractions::{BrokerStore, Clock, Join, JoinCodeRow, MachineKind, MachineRow};
use lr_core::{JobId, JobState, Timestamp};
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

const LAPTOP: &str = "laptop";

async fn pending(h: &Harness, client: &str) -> usize {
    h.store.pending_results(client).await.unwrap().len()
}

async fn hub_with_offline_runner(h: &Harness) -> Conn {
    let r = h.member_runner("r0", "10.0.0.7", linux()).await;
    r.close().await;
    let mut hub = h.member_hub(LAPTOP, "", "10.0.0.8").await;
    hub.until_runner("r0", false).await;
    hub
}

async fn running_on_r1(h: &Harness, hub: &mut Conn) -> Conn {
    let mut r = h.member_runner("r1", "10.0.0.5", linux()).await;
    hub.send(&order("j1", req(512, 1, 3600))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    r.acked(1).await;
    hub.started_notice().await;
    r
}

#[tokio::test]
async fn revoked_client_cancels_waiting_orders_with_client_removed() {
    let h = Harness::start("");
    let mut hub = hub_with_offline_runner(&h).await;
    let mut other = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    other.send(&order("j1", req(512, 1, 60))).await;
    other.accepted().await;

    h.handle.revoked(LAPTOP).await;

    assert!(hub.closed().await, "the revoked hub stays connected");
    let row = h.job_row(LAPTOP, "j1").await.unwrap();
    assert_eq!(row.job.state, JobState::Cancelled);
    assert_eq!(row.job.cancel_reason.as_deref(), Some("client_removed"));
    assert_eq!(row.job.cancelled_by.as_deref(), Some("broker"));
    assert_eq!(
        h.job_row("local:alex", "j1").await.unwrap().job.state,
        JobState::Queued
    );
    let mut r = h.member_runner("r1", "10.0.0.5", linux()).await;
    let a = r.assign().await;
    assert_eq!(a.spec.client, "local:alex");
    r.quiet().await;
}

#[tokio::test]
async fn revoked_client_outbox_is_deleted() {
    let h = Harness::start("");
    let mut hub = hub_with_offline_runner(&h).await;
    let mut other = h.local_hub("alex").await;
    for (c, job) in [(&mut hub, "j0"), (&mut other, "j0")] {
        c.send(&order(job, req(512, 1, 60))).await;
        c.accepted().await;
        c.send(&cancel(job, 2)).await;
    }
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    other.sync().await;
    hub.sync().await;
    assert!(pending(&h, LAPTOP).await > 0);
    let others = pending(&h, "local:alex").await;
    assert!(others > 0);

    h.handle.revoked(LAPTOP).await;

    h.until_state(LAPTOP, "j1", JobState::Cancelled).await;
    assert_eq!(pending(&h, LAPTOP).await, 0);
    assert_eq!(pending(&h, "local:alex").await, others);
}

#[tokio::test]
async fn revoked_client_running_job_is_cancelled_when_orphans_running_is_cancel() {
    let h = Harness::start("[orphans]\nrunning = \"cancel\"\n");
    let mut hub = h.member_hub(LAPTOP, "", "10.0.0.8").await;
    let mut r = running_on_r1(&h, &mut hub).await;

    h.handle.revoked(LAPTOP).await;

    let c = loop {
        if let BrokerToRunner::Cancel(c) = r.runner().await {
            break c;
        }
    };
    assert_eq!(
        (c.job, c.code),
        (JobId("j1".into()), ReasonCode::ClientRemoved)
    );
    let row = h.until_state(LAPTOP, "j1", JobState::Cancelling).await;
    assert_eq!(row.job.cancel_reason.as_deref(), Some("client_removed"));
}

#[tokio::test]
async fn revoked_client_running_job_finishes_by_default_without_outbox() {
    let h = Harness::start("");
    let mut hub = h.member_hub(LAPTOP, "", "10.0.0.8").await;
    let mut r = running_on_r1(&h, &mut hub).await;

    h.handle.revoked(LAPTOP).await;

    r.quiet().await;
    assert_eq!(
        h.job_row(LAPTOP, "j1").await.unwrap().job.state,
        JobState::Running
    );
    r.send(&result("j1", 2, JobState::Passed, 0)).await;
    r.acked(2).await;
    h.until_state(LAPTOP, "j1", JobState::Passed).await;
    assert_eq!(pending(&h, LAPTOP).await, 0);
}

#[tokio::test]
async fn waiting_order_of_a_key_revoked_before_a_crash_is_cancelled_at_start() {
    let h = Harness::start("");
    let mut hub = hub_with_offline_runner(&h).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    h.store.revoke(LAPTOP, Timestamp(START)).await.unwrap();

    let h = h.restart().await;

    let row = h.until_state(LAPTOP, "j1", JobState::Cancelled).await;
    assert_eq!(row.job.cancel_reason.as_deref(), Some("client_removed"));
    assert_eq!(pending(&h, LAPTOP).await, 0);
    let mut r = h.member_runner("r1", "10.0.0.5", linux()).await;
    r.quiet().await;
}

async fn rejoin(h: &Harness, name: &str) -> Conn {
    let code_hash = format!("h2-{name}");
    let key = format!("pk2-{name}");
    h.store
        .issue_code(JoinCodeRow {
            code_hash: code_hash.clone(),
            kind: MachineKind::Hub,
            name: None,
            label: None,
            uses_left: 1,
            issued_at: Timestamp(0),
            expires_at: Timestamp(u64::MAX),
            redeemed_at: None,
            machine_name: None,
        })
        .await
        .unwrap();
    let at = h.clock.now();
    h.store
        .redeem(Join {
            code_hash,
            at,
            machine: MachineRow {
                name: name.into(),
                machine_id: format!("mid2-{name}"),
                kind: MachineKind::Hub,
                label: String::new(),
                public_key: key.clone(),
                registered_at: at,
                last_seen: None,
                revoked_at: None,
            },
        })
        .await
        .unwrap();
    let mut c = h.dial(member_peer(&key, "10.0.0.8")).await;
    c.send(&hello(name, None)).await;
    c.welcome().await;
    c
}

#[tokio::test]
async fn rejoined_name_gets_no_outcome_of_runs_from_before_the_revoke() {
    let h = Harness::start("");
    let mut hub = h.member_hub(LAPTOP, "", "10.0.0.8").await;
    let mut r = running_on_r1(&h, &mut hub).await;
    h.handle.revoked(LAPTOP).await;
    h.clock.advance(std::time::Duration::from_secs(1));
    let mut hub = rejoin(&h, LAPTOP).await;

    r.send(&result("j1", 2, JobState::Passed, 0)).await;
    r.acked(2).await;
    h.until_state(LAPTOP, "j1", JobState::Passed).await;
    assert_eq!(pending(&h, LAPTOP).await, 0);

    hub.send(&order("j3", req(512, 1, 3600))).await;
    hub.accepted().await;
    assert_eq!(r.assign().await.job, JobId("j3".into()));
    r.send(&started("j3", 1)).await;
    r.acked(1).await;
    r.send(&result("j3", 2, JobState::Passed, 0)).await;
    r.acked(2).await;
    let first = loop {
        if let BrokerToHub::JobResult(res) = hub.hub().await {
            break res;
        }
    };
    assert_eq!(first.job, JobId("j3".into()));
}

#[tokio::test]
async fn revoked_client_gets_no_results_after_a_broker_restart() {
    let h = Harness::start("");
    let mut hub = h.member_hub(LAPTOP, "", "10.0.0.8").await;
    let r = running_on_r1(&h, &mut hub).await;
    h.handle.revoked(LAPTOP).await;
    r.close().await;

    let h = h.restart().await;
    let mut r = h.dial(member_peer("pk-r1", "10.0.0.5")).await;
    r.send(&hello("r1", Some(linux()))).await;
    r.welcome().await;
    r.send(&result("j1", 2, JobState::Passed, 0)).await;
    r.acked(2).await;
    h.until_state(LAPTOP, "j1", JobState::Passed).await;
    assert_eq!(pending(&h, LAPTOP).await, 0);
}

#[tokio::test]
async fn a_run_of_a_revoked_key_parked_by_a_failed_requeue_ends_client_removed() {
    let h = Harness::start("");
    let mut hub = h.member_hub(LAPTOP, "", "10.0.0.8").await;
    let mut r = h.member_runner("r1", "10.0.0.5", linux()).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    common::faulty::FaultyStore::set(&h.faults.fail_requeue);
    r.close().await;
    hub.until_runner("r1", false).await;
    h.handle.revoked(LAPTOP).await;
    common::faulty::FaultyStore::clear(&h.faults.fail_requeue);
    h.clock.advance(std::time::Duration::from_secs(1));
    h.poke().await;
    let row = h.until_state(LAPTOP, "j1", JobState::Cancelled).await;
    assert_eq!(row.job.cancel_reason.as_deref(), Some("client_removed"));
}
