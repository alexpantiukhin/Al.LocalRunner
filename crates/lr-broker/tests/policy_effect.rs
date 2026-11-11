mod common;

use common::harness::*;
use lr_core::{JobId, JobState};
use lr_proto::*;

const ALEX: &str = "local:alex";

#[tokio::test]
async fn reserve_wait_says_n_of_m_cores_reserved_by_the_named_rule() {
    let h = Harness::start(
        "[[rule]]\nname = \"ci-reserve\"\nwhen = { client_label = \"ci\" }\n\
         reserve = { cpu = 2 }\n",
    );
    let mut r1 = h.member_runner("r1", "10.0.0.7", linux()).await;
    let _r2 = h.member_runner("r2", "10.0.0.8", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(1024, 4, 60))).await;
    assert_eq!(hub.accepted().await.waiting, None);
    r1.assign().await;
    r1.send(&started("j1", 1)).await;
    hub.send(&order("j2", req(1024, 4, 60))).await;
    assert_eq!(
        hub.accepted().await.waiting.as_deref(),
        Some("правило ci-reserve: 2 из 8 ядер зарезервированы")
    );
}

#[tokio::test]
async fn reload_does_not_revisit_accepted_or_running_orders() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("running", req(512, 1, 1200))).await;
    hub.accepted().await;
    assert_eq!(r.assign().await.job, JobId("running".into()));
    r.send(&started("running", 1)).await;
    hub.send(&order("queued", req(512, 1, 1200))).await;
    assert!(hub.accepted().await.waiting.is_some(), "the runner is busy");
    h.handle
        .reload_policy(&policy("[limits]\nmax_job_timeout_s = 600\n"))
        .await
        .expect("reload applies");
    hub.send(&order("new", req(512, 1, 1200))).await;
    assert_eq!(
        hub.rejected().await.code,
        ReasonCode::Rejected(RejectReason::Timeout),
        "the new policy applies to new orders"
    );
    r.send(&result("running", 2, JobState::Passed, 0)).await;
    let a = r.assign().await;
    assert_eq!(
        (a.job.0.as_str(), a.spec.requirements.timeout_s),
        ("queued", 1200),
        "the running job was not touched and the queued one keeps its decision"
    );
    let done = h.until_state(ALEX, "running", JobState::Passed).await;
    assert_eq!(done.job.cancel_reason, None);
}
