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
