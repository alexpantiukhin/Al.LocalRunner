mod common;

use common::harness::*;
use lr_core::JobId;
use lr_proto::*;

const ALLOW: &str = "[[services.allow]]\nlabel = \"ci\"\nnames = [\"build-daemon\"]\n";

fn with_service() -> WireCapabilities {
    let mut c = linux();
    c.services
        .insert("build-daemon".into(), WireServiceStatus::Available);
    c
}

fn needs_service(mem_mb: u64) -> lr_core::Requirements {
    let mut r = req(mem_mb, 1, 60);
    r.services = vec!["build-daemon".into()];
    r
}

fn report(state: ServiceState) -> RunnerToBroker {
    RunnerToBroker::ServicesReport(ServicesReport {
        services: vec![ServiceReport {
            name: "build-daemon".into(),
            state,
            mem_mb: 2048,
            remaining_s: Some(600),
        }],
    })
}

#[tokio::test]
async fn service_not_allowed_for_label_rejected() {
    let h = Harness::start(ALLOW);
    let _r = h.member_runner("r1", "10.0.0.7", with_service()).await;
    let mut agent = h.member_hub("laptop", "agent", "10.0.0.20").await;
    agent.send(&order("j1", needs_service(512))).await;
    let r = agent.rejected().await;
    assert_eq!(r.code.as_str(), "rejected_service_not_allowed");
    assert_eq!(r.detail, "сервис build-daemon не разрешён ярлыку agent");
    let mut ci = h.member_hub("ci-box", "ci", "10.0.0.21").await;
    ci.send(&order("j1", needs_service(512))).await;
    ci.accepted().await;
}

#[tokio::test]
async fn any_label_may_request_services_without_an_allow_list() {
    let h = Harness::start("");
    let _r = h.member_runner("r1", "10.0.0.7", with_service()).await;
    let mut agent = h.member_hub("laptop", "agent", "10.0.0.20").await;
    agent.send(&order("j1", needs_service(512))).await;
    agent.accepted().await;
}

#[tokio::test]
async fn capabilities_update_replaces_what_the_runner_offers() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    let mut android = req(512, 1, 60);
    android.platform = Some("android".into());
    hub.send(&order("a1", android.clone())).await;
    assert_eq!(
        hub.rejected().await.code.as_str(),
        "rejected_no_matching_runner"
    );
    let mut more = linux();
    more.platforms.insert("android".into());
    r.send(&RunnerToBroker::CapabilitiesUpdate(CapabilitiesUpdate {
        capabilities: more,
    }))
    .await;
    hub.sync().await;
    hub.send(&order("a2", android)).await;
    hub.accepted().await;
    assert_eq!(r.assign().await.job, JobId("a2".into()));
    let rows = lr_abstractions::BrokerStore::runners(h.store.as_ref())
        .await
        .unwrap();
    assert!(rows[0].platforms.contains("android"));
}

#[tokio::test]
async fn services_report_is_shown_on_the_runners_list() {
    let h = Harness::start("");
    let r = h.member_runner("r1", "10.0.0.7", with_service()).await;
    r.send(&report(ServiceState::Idle)).await;
    let mut hub = h.local_hub("alex").await;
    let ReadReply::Runners { runners } = hub.read(ReadRequest::Runners).await else {
        panic!("runners reply expected");
    };
    assert_eq!(
        runners[0].services,
        [ServiceReport {
            name: "build-daemon".into(),
            state: ServiceState::Idle,
            mem_mb: 2048,
            remaining_s: Some(600),
        }]
    );
}

#[tokio::test]
async fn a_report_that_only_counts_down_the_lifetime_is_no_service_change() {
    let mut h = Harness::start("");
    let r = h.member_runner("r1", "10.0.0.7", with_service()).await;
    r.send(&report(ServiceState::Idle)).await;
    let RunnerToBroker::ServicesReport(mut later) = report(ServiceState::Idle) else {
        unreachable!()
    };
    later.services[0].remaining_s = Some(590);
    r.send(&RunnerToBroker::ServicesReport(later)).await;
    r.send(&report(ServiceState::Busy)).await;
    h.local_hub("alex").await.sync().await;
    let mut changes = Vec::new();
    while let Ok(lr_broker::BusEvent { event, .. }) = h.events.try_recv() {
        if let QueueEvent::ServiceChanged { service, .. } = event {
            changes.push(service.state);
        }
    }
    assert_eq!(changes, [ServiceState::Idle, ServiceState::Busy]);
}

async fn head_lacks_memory(h: &Harness) -> Conn {
    let mut busy = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("big", req(12288, 1, 60))).await;
    hub.accepted().await;
    busy.assign().await;
    let warm = h.member_runner("r2", "10.0.0.8", with_service()).await;
    warm.send(&report(ServiceState::Idle)).await;
    hub.sync().await;
    hub.send(&order("next", req(8192, 1, 60))).await;
    hub.accepted().await;
    warm
}

#[tokio::test]
async fn idle_service_is_stopped_when_the_head_lacks_memory() {
    let h = Harness::start("");
    let mut warm = head_lacks_memory(&h).await;
    match warm.runner().await {
        BrokerToRunner::StopService(s) => assert_eq!(s.name, "build-daemon"),
        other => panic!("expected stop_service, got {other:?}"),
    }
    warm.quiet().await;
}

#[tokio::test]
async fn idle_services_are_kept_when_eviction_is_off() {
    let h = Harness::start("[services]\nevict_idle = false\n");
    let mut warm = head_lacks_memory(&h).await;
    warm.quiet().await;
}

fn holding(state: ServiceState, mem_mb: u64) -> RunnerToBroker {
    RunnerToBroker::ServicesReport(ServicesReport {
        services: vec![ServiceReport {
            name: "build-daemon".into(),
            state,
            mem_mb,
            remaining_s: Some(600),
        }],
    })
}

#[tokio::test]
async fn live_service_memory_stays_in_the_pool_budget_until_the_service_stops() {
    let h = Harness::start("");
    let mut warm = h.member_runner("r1", "10.0.0.7", with_service()).await;
    warm.send(&holding(ServiceState::Idle, 10240)).await;
    let mut hub = h.local_hub("alex").await;
    hub.sync().await;
    hub.send(&order("j1", req(8192, 1, 60))).await;
    hub.accepted().await;
    match warm.runner().await {
        BrokerToRunner::StopService(s) => assert_eq!(s.name, "build-daemon"),
        other => panic!("expected stop_service before any assign, got {other:?}"),
    }
    warm.send(&RunnerToBroker::ServicesReport(ServicesReport {
        services: Vec::new(),
    }))
    .await;
    assert_eq!(warm.assign().await.job, JobId("j1".into()));
}

#[tokio::test]
async fn a_busy_service_is_never_evicted_but_its_memory_is_held() {
    let h = Harness::start("");
    let mut warm = h.member_runner("r1", "10.0.0.7", with_service()).await;
    warm.send(&holding(ServiceState::Busy, 10240)).await;
    let mut hub = h.local_hub("alex").await;
    hub.sync().await;
    hub.send(&order("j1", req(8192, 1, 60))).await;
    hub.accepted().await;
    warm.quiet().await;
    warm.send(&holding(ServiceState::Stopped, 10240)).await;
    assert_eq!(warm.assign().await.job, JobId("j1".into()));
}

fn declares_service(mem_mb: u64) -> WireCapabilities {
    let mut c = with_service();
    c.service_mem_mb.insert("build-daemon".into(), mem_mb);
    c
}

#[tokio::test]
async fn an_order_with_a_service_waits_when_the_pool_fits_only_the_job() {
    let h = Harness::start("");
    let mut busy = h.member_runner("r2", "10.0.0.8", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("plain", req(4000, 1, 60))).await;
    hub.accepted().await;
    assert_eq!(busy.assign().await.job, JobId("plain".into()));
    let mut r = h
        .member_runner("r1", "10.0.0.7", declares_service(4096))
        .await;
    hub.sync().await;
    hub.send(&order("svc", needs_service(10000))).await;
    hub.accepted().await;
    r.quiet().await;
}

#[tokio::test]
async fn a_warm_service_on_the_runner_is_not_reserved_twice() {
    let h = Harness::start("");
    let mut r = h
        .member_runner("r1", "10.0.0.7", declares_service(4096))
        .await;
    r.send(&holding(ServiceState::Idle, 4096)).await;
    let mut hub = h.local_hub("alex").await;
    hub.sync().await;
    hub.send(&order("j1", needs_service(12000))).await;
    hub.accepted().await;
    assert_eq!(r.assign().await.job, JobId("j1".into()));
}

#[tokio::test]
async fn an_assigned_service_holds_its_memory_before_the_runner_reports_it() {
    let h = Harness::start("");
    let mut r1 = h
        .member_runner("r1", "10.0.0.7", declares_service(4096))
        .await;
    let mut r2 = h.member_runner("r2", "10.0.0.8", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("with-service", needs_service(8000))).await;
    hub.accepted().await;
    assert_eq!(r1.assign().await.job, JobId("with-service".into()));
    hub.send(&order("plain", req(6000, 1, 60))).await;
    hub.accepted().await;
    r2.quiet().await;
}

#[tokio::test]
async fn reserve_rules_count_the_memory_of_live_services() {
    let h = Harness::start(
        "[[rule]]\nname = \"ci-reserve\"\nwhen = { client_label = \"ci\" }\n\
         reserve = { mem_mb = 8192 }\n",
    );
    let mut warm = h
        .member_runner("r1", "10.0.0.7", declares_service(4096))
        .await;
    warm.send(&holding(ServiceState::Busy, 4096)).await;
    let mut r2 = h.member_runner("r2", "10.0.0.8", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.sync().await;
    hub.send(&order("j1", req(6000, 1, 60))).await;
    hub.accepted().await;
    r2.quiet().await;
    warm.quiet().await;
}
