mod common;

use common::harness::*;
use lr_core::JobId;
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
    let mut r = req(mem_mb, 1, 60);
    r.services = vec!["build-daemon".into()];
    r
}

fn idle(mem_mb: u64) -> RunnerToBroker {
    RunnerToBroker::ServicesReport(ServicesReport {
        services: vec![ServiceReport {
            name: "build-daemon".into(),
            state: ServiceState::Idle,
            mem_mb,
            remaining_s: Some(600),
        }],
    })
}

#[tokio::test]
async fn an_order_that_never_fits_with_its_services_is_rejected_on_pool_memory() {
    let h = Harness::start("");
    let _r = h
        .member_runner("r1", "10.0.0.7", declares(65536, 4096))
        .await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("big", needs_service(14000))).await;
    let r = hub.rejected().await;
    assert_eq!(r.code.as_str(), "rejected_pool_memory");
    assert_eq!(
        r.detail,
        "нужно 18096 МБ, максимум в пуле 16384 (джоба 14000 МБ + сервисы 4096 МБ)"
    );
}

#[tokio::test]
async fn a_runner_that_fits_the_job_but_not_its_services_does_not_match() {
    let h = Harness::start("");
    let _r = h
        .member_runner("r1", "10.0.0.7", declares(8192, 4096))
        .await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", needs_service(6000))).await;
    let r = hub.rejected().await;
    assert_eq!(r.code.as_str(), "rejected_no_matching_runner");
    assert!(
        r.detail.contains("r1: нужно 10096 МБ, у раннера 8192"),
        "{}",
        r.detail
    );
    assert!(r.detail.contains("джоба 6000 МБ"), "{}", r.detail);
}

#[tokio::test]
async fn a_quota_below_the_job_with_its_services_rejects_instead_of_waiting() {
    let h = Harness::start("[[rule]]\nname = \"small\"\nquota = { max_mem_mb = 8192 }\n");
    let _r = h
        .member_runner("r1", "10.0.0.7", declares(65536, 4096))
        .await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", needs_service(6000))).await;
    let r = hub.rejected().await;
    assert_eq!(r.code.as_str(), "rejected_quota");
    hub.send(&order("j2", req(6000, 1, 60))).await;
    hub.accepted().await;
}

#[tokio::test]
async fn an_order_that_never_fits_does_not_evict_idle_services() {
    let h = Harness::start("");
    let mut warm = h
        .member_runner("r1", "10.0.0.7", declares(65536, 4096))
        .await;
    warm.send(&idle(4096)).await;
    let mut hub = h.local_hub("alex").await;
    hub.sync().await;
    hub.send(&order("big", needs_service(14000))).await;
    hub.rejected().await;
    warm.quiet().await;
}

#[tokio::test]
async fn a_warm_service_on_one_runner_is_reused_although_another_runner_is_cold() {
    let h = Harness::start("");
    let mut warm = h
        .member_runner("r1", "10.0.0.7", declares(65536, 4096))
        .await;
    warm.send(&idle(4096)).await;
    let mut cold = h
        .member_runner("r2", "10.0.0.8", declares(65536, 4096))
        .await;
    let mut hub = h.local_hub("alex").await;
    hub.sync().await;
    hub.send(&order("j1", needs_service(12000))).await;
    hub.accepted().await;
    match warm.runner().await {
        BrokerToRunner::Assign(a) => assert_eq!(a.job, JobId("j1".into())),
        other => panic!("expected assign on the warm runner, got {other:?}"),
    }
    cold.quiet().await;
}

#[tokio::test]
async fn a_preferred_cold_runner_does_not_take_an_order_its_service_would_overcommit() {
    let h = Harness::start("");
    let mut cold = h
        .member_runner("r1", "10.0.0.7", declares(65536, 4096))
        .await;
    let mut warm = h
        .member_runner("r2", "10.0.0.8", declares(65536, 4096))
        .await;
    warm.send(&idle(4096)).await;
    let mut hub = h.local_hub("alex").await;
    hub.sync().await;
    hub.send(&order("j1", needs_service(12000))).await;
    hub.accepted().await;
    match warm.runner().await {
        BrokerToRunner::Assign(a) => assert_eq!(a.job, JobId("j1".into())),
        other => panic!("expected assign on the warm runner, got {other:?}"),
    }
    cold.quiet().await;
}
