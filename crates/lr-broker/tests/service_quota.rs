mod common;

use common::harness::*;
use lr_core::{JobId, JobState};
use lr_proto::*;

fn declares(mem_mb: u64, service_mb: u64) -> WireCapabilities {
    let mut c = linux();
    c.mem_mb = mem_mb;
    c.services
        .insert("build-daemon".into(), WireServiceStatus::Available);
    c.service_mem_mb.insert("build-daemon".into(), service_mb);
    c
}

fn needs_service(mem_mb: u64) -> lr_core::Requirements {
    let mut r = req(mem_mb, 1, 600);
    r.services = vec!["build-daemon".into()];
    r
}

#[tokio::test]
async fn a_queued_order_counts_its_service_memory_against_the_quota() {
    let h = Harness::start("[[rule]]\nname = \"small\"\nquota = { max_mem_mb = 12000 }\n");
    let mut r1 = h
        .member_runner("r1", "10.0.0.7", declares(65536, 4096))
        .await;
    let mut r2 = h
        .member_runner("r2", "10.0.0.8", declares(65536, 4096))
        .await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", needs_service(4000))).await;
    assert_eq!(hub.accepted().await.waiting, None);
    let first = r1.assign().await;
    assert_eq!(first.job, JobId("j1".into()));

    hub.send(&order("j2", needs_service(1000))).await;
    let waiting = hub.accepted().await.waiting.expect("j2 waits");
    assert!(waiting.contains("small"), "{waiting}");
    r2.quiet().await;

    r1.send(&started("j1", 1)).await;
    r1.send(&result("j1", 2, JobState::Passed, 0)).await;
    r1.acked(2).await;
    let next = tokio::select! {
        a = r1.assign() => a,
        a = r2.assign() => a,
    };
    assert_eq!(next.job, JobId("j2".into()));
}

#[tokio::test]
async fn without_services_the_same_orders_fit_the_quota_together() {
    let h = Harness::start("[[rule]]\nname = \"small\"\nquota = { max_mem_mb = 12000 }\n");
    let mut r1 = h
        .member_runner("r1", "10.0.0.7", declares(65536, 4096))
        .await;
    let mut r2 = h
        .member_runner("r2", "10.0.0.8", declares(65536, 4096))
        .await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(4000, 1, 600))).await;
    hub.accepted().await;
    r1.assign().await;
    hub.send(&order("j2", req(1000, 1, 600))).await;
    assert_eq!(hub.accepted().await.waiting, None);
    assert_eq!(r2.assign().await.job, JobId("j2".into()));
}
