mod common;

use common::harness::*;
use lr_core::{JobId, Requirements};
use lr_proto::*;

fn overridden(job: &str, requirements: Requirements, over: ResourceOverride) -> HubToBroker {
    let HubToBroker::Order(mut o) = order(job, requirements) else {
        unreachable!()
    };
    o.spec.overrides = over;
    HubToBroker::Order(o)
}

#[tokio::test]
async fn runner_smaller_than_the_override_does_not_match_with_figures() {
    let h = Harness::start("");
    let _r = h
        .member_runner("r-small", "10.0.0.7", caps(&["linux"], 4096, 2))
        .await;
    let mut hub = h.local_hub("alex").await;
    let mem = ResourceOverride {
        mem_mb: Some(6144),
        cpu: None,
    };
    hub.send(&overridden("j1", req(6144, 1, 60), mem)).await;
    let r = hub.rejected().await;
    assert_eq!(r.code, ReasonCode::Rejected(RejectReason::NoMatchingRunner));
    assert!(
        r.detail.contains("r-small") && r.detail.contains("нужно 6144 МБ, у раннера 4096"),
        "{}",
        r.detail
    );
    let cpu = ResourceOverride {
        mem_mb: None,
        cpu: Some(4),
    };
    hub.send(&overridden("j2", req(512, 4, 60), cpu)).await;
    let r = hub.rejected().await;
    assert_eq!(r.code, ReasonCode::Rejected(RejectReason::NoMatchingRunner));
    assert!(
        r.detail.contains("нужно 4 CPU, у раннера 2"),
        "{}",
        r.detail
    );
    let lower = ResourceOverride {
        mem_mb: Some(2048),
        cpu: Some(2),
    };
    hub.send(&overridden("j3", req(2048, 2, 60), lower)).await;
    assert_eq!(hub.accepted().await.job, JobId("j3".into()));
}

#[tokio::test]
async fn pool_budget_uses_order_requirements_not_step_values() {
    let h = Harness::start("");
    let mut r1 = h
        .member_runner("r1", "10.0.0.7", caps(&["linux"], 16384, 8))
        .await;
    let mut hub = h.local_hub("alex").await;
    let over = ResourceOverride {
        mem_mb: Some(4096),
        cpu: Some(2),
    };
    let HubToBroker::Order(mut first) = overridden("j1", req(4096, 2, 60), over) else {
        unreachable!()
    };
    first.spec.steps[0].mem_mb = Some(12000);
    first.spec.steps[0].cpu = Some(6);
    hub.send(&HubToBroker::Order(first)).await;
    assert_eq!(hub.accepted().await.waiting, None);
    let a1 = r1.assign().await;
    assert_eq!(a1.job, JobId("j1".into()));
    r1.send(&started("j1", 1)).await;
    let mut r2 = h
        .member_runner("r2", "10.0.0.8", caps(&["linux"], 16384, 8))
        .await;
    hub.send(&order("j2", req(12288, 6, 60))).await;
    assert_eq!(
        hub.accepted().await.waiting,
        None,
        "the pool holds 4096 MB and 2 CPU for j1, not the 12000 MB of its step"
    );
    assert_eq!(r2.assign().await.job, JobId("j2".into()));
    r2.send(&started("j2", 1)).await;
    let mut r3 = h
        .member_runner("r3", "10.0.0.9", caps(&["linux"], 16384, 8))
        .await;
    hub.send(&order("j3", req(512, 1, 60))).await;
    assert!(
        hub.accepted().await.waiting.is_some(),
        "16384 MB and 8 CPU are taken"
    );
    r3.quiet().await;
}

#[tokio::test]
async fn trimmed_order_still_skips_a_runner_with_a_lower_ceiling() {
    let h = Harness::start("[limits]\nmax_job_timeout_s = 600\ntimeout_above_max = \"trim\"\n");
    let mut low = linux();
    low.max_job_timeout_s = Some(300);
    let _low = h.member_runner("r-low", "10.0.0.7", low).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 7200))).await;
    let r = hub.rejected().await;
    assert_eq!(r.code, ReasonCode::Rejected(RejectReason::NoMatchingRunner));
    assert!(
        r.detail.contains("r-low")
            && r.detail
                .contains("нужен таймаут 600 с, потолок раннера 300 с"),
        "{}",
        r.detail
    );
    let mut fit = linux();
    fit.max_job_timeout_s = Some(900);
    let mut ok = h.member_runner("r-ok", "10.0.0.8", fit).await;
    hub.send(&order("j2", req(512, 1, 7200))).await;
    assert_eq!(hub.accepted().await.timeout_trimmed_to, Some(600));
    let a = ok.assign().await;
    assert_eq!(a.job, JobId("j2".into()));
    assert_eq!(a.spec.requirements.timeout_s, 600);
}
