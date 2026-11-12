mod common;

use common::faulty::FaultyStore;
use common::harness::*;
use lr_core::{JobId, JobState};
use lr_proto::*;

const ALLOW: &str = "[[secrets.allow]]\nlabel = \"ci\"\nnames = [\"registry_auth\"]\n\
[[services.allow]]\nlabel = \"ci\"\nnames = [\"build-daemon\"]\n";

#[tokio::test]
async fn label_checks_of_secrets_and_services_come_before_runner_checks() {
    let h = Harness::start(ALLOW);
    let mut hub = h.member_hub("laptop", "agent", "10.0.0.20").await;
    let mut secret = req(512, 1, 60);
    secret.secrets.insert("registry_auth".into());
    hub.send(&order("s1", secret)).await;
    let r = hub.rejected().await;
    assert_eq!(r.code.as_str(), "rejected_secret_not_allowed");
    let mut service = req(512, 1, 60);
    service.services = vec!["build-daemon".into()];
    hub.send(&order("v1", service.clone())).await;
    let r = hub.rejected().await;
    assert_eq!(r.code.as_str(), "rejected_service_not_allowed");

    let mut ci = h.member_hub("ci-box", "ci", "10.0.0.21").await;
    ci.send(&order("v1", service)).await;
    let r = ci.rejected().await;
    assert_eq!(r.code.as_str(), "rejected_no_matching_runner");
}

#[tokio::test]
async fn a_store_failure_while_taking_a_cancel_answers_unavailable_and_a_retry_works() {
    let h = Harness::start("");
    let mut hub = h.local_hub("alex").await;
    let r = h.member_runner("r0", "10.0.0.7", linux()).await;
    r.close().await;
    hub.until_runner("r0", false).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;

    FaultyStore::set(&h.faults.fail_accept_seq);
    hub.send(&cancel("j1", 2)).await;
    let err = loop {
        if let BrokerToHub::Error(e) = hub.hub().await {
            break e;
        }
    };
    assert_eq!(err.code, ProtocolErrorCode::Unavailable);
    assert!(err.message.contains("j1"), "{}", err.message);
    assert!(h
        .log
        .records()
        .iter()
        .any(|r| r.message == "storage failed: could not record the cancel"));
    assert_eq!(
        h.job_row("local:alex", "j1").await.unwrap().job.state,
        JobState::Queued
    );

    FaultyStore::clear(&h.faults.fail_accept_seq);
    hub.send(&cancel("j1", 2)).await;
    h.until_state("local:alex", "j1", JobState::Cancelled).await;
}

#[tokio::test]
async fn a_waiting_order_skips_an_offline_runner_for_another_suitable_one() {
    let h = Harness::start("[[rule]]\nname = \"one-at-a-time\"\nquota = { max_mem_mb = 600 }\n");
    let mut r1 = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut r2 = h.member_runner("r2", "10.0.0.8", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    let first_on_r1 = tokio::select! {
        a = r1.assign() => { assert_eq!(a.job, JobId("j1".into())); true }
        a = r2.assign() => { assert_eq!(a.job, JobId("j1".into())); false }
    };
    let (mut busy, free, free_name) = if first_on_r1 {
        (r1, r2, "r2")
    } else {
        (r2, r1, "r1")
    };
    hub.send(&order("j2", req(512, 1, 60))).await;
    let waiting = hub.accepted().await.waiting.expect("j2 waits by the quota");
    assert!(waiting.contains("one-at-a-time"), "{waiting}");
    free.close().await;
    hub.until_runner(free_name, false).await;
    busy.send(&started("j1", 1)).await;
    busy.send(&result("j1", 2, JobState::Passed, 0)).await;
    busy.acked(2).await;
    assert_eq!(
        busy.assign().await.job,
        JobId("j2".into()),
        "the runner that was free when j2 queued went offline, so j2 goes to the other one"
    );
}

#[tokio::test]
async fn a_policy_reload_leaves_running_jobs_as_they_are() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(8000, 4, 3600))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    r.acked(1).await;

    let smaller = format!(
        "[pool]\nmem_mb = 4096\ncpu = 2\n{USERS}[limits]\nmax_job_timeout_s = 60\n\
         timeout_above_max = \"reject\"\n"
    );
    h.handle.reload_policy(&smaller).await.expect("reload");
    hub.sync().await;
    r.quiet().await;
    assert_eq!(
        h.job_row("local:alex", "j1").await.unwrap().job.state,
        JobState::Running
    );
    r.send(&result("j1", 2, JobState::Passed, 0)).await;
    r.acked(2).await;
    h.until_state("local:alex", "j1", JobState::Passed).await;
}
