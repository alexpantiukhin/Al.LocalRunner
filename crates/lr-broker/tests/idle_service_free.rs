mod common;

use common::harness::*;
use lr_core::JobId;
use lr_proto::*;

fn with_service() -> WireCapabilities {
    let mut c = linux();
    c.services
        .insert("build-daemon".into(), WireServiceStatus::Available);
    c.tags.insert("db".into());
    c
}

fn services(mem_mb: Option<u64>) -> RunnerToBroker {
    in_state(ServiceState::Idle, mem_mb)
}

fn in_state(state: ServiceState, mem_mb: Option<u64>) -> RunnerToBroker {
    RunnerToBroker::ServicesReport(ServicesReport {
        services: mem_mb
            .map(|mem_mb| ServiceReport {
                name: "build-daemon".into(),
                state,
                mem_mb,
                remaining_s: Some(600),
            })
            .into_iter()
            .collect(),
    })
}

async fn head_behind_an_idle_service(policy: &str) -> (Harness, Conn, Conn) {
    let h = Harness::start(policy);
    let warm = h.member_runner("r1", "10.0.0.7", with_service()).await;
    warm.send(&services(Some(12288))).await;
    let mut hub = h.local_hub("alex").await;
    hub.sync().await;
    hub.send(&order("head", req(13000, 1, 60))).await;
    assert!(hub.accepted().await.waiting.is_some());
    (h, warm, hub)
}

#[tokio::test]
async fn with_eviction_the_idle_service_is_free_for_the_reservation_so_nothing_jumps_ahead() {
    let (_h, mut warm, mut hub) = head_behind_an_idle_service("").await;
    match warm.runner().await {
        BrokerToRunner::StopService(s) => assert_eq!(s.name, "build-daemon"),
        other => panic!("expected stop_service, got {other:?}"),
    }
    hub.send(&order("small", req(4096, 1, 3600))).await;
    assert!(
        hub.accepted().await.waiting.is_some(),
        "4096 MB fit now, but would leave 12288 MB where the head needs 13000 once the service stops"
    );
    warm.quiet().await;
    warm.send(&in_state(ServiceState::Stopping, Some(12288)))
        .await;
    hub.sync().await;
    warm.quiet().await;
    warm.send(&services(None)).await;
    assert_eq!(warm.assign().await.job, JobId("head".into()));
}

#[tokio::test]
async fn without_a_head_on_memory_nothing_is_evicted_and_holes_are_filled() {
    let h = Harness::start("");
    let mut warm = h.member_runner("r1", "10.0.0.7", with_service()).await;
    warm.send(&services(Some(12288))).await;
    let mut hub = h.local_hub("alex").await;
    hub.sync().await;
    let mut holder = req(512, 1, 600);
    holder.exclusive.insert("db".into());
    hub.send(&order("holder", holder.clone())).await;
    assert_eq!(hub.accepted().await.waiting, None);
    assert_eq!(warm.assign().await.job, JobId("holder".into()));
    let mut free = h.member_runner("r2", "10.0.0.8", with_service()).await;
    hub.until_runner("r2", true).await;
    hub.send(&order("tagged", holder)).await;
    assert!(hub.accepted().await.waiting.is_some());
    hub.send(&order("big", req(13000, 1, 60))).await;
    assert!(hub.accepted().await.waiting.is_some());
    hub.send(&order("small", req(3000, 1, 60))).await;
    assert_eq!(
        hub.accepted().await.waiting,
        None,
        "the head waits for a tag, so no service is stopped and the small order fills the hole"
    );
    assert_eq!(free.assign().await.job, JobId("small".into()));
    warm.quiet().await;
    free.quiet().await;
}

#[tokio::test]
async fn without_eviction_the_idle_service_holds_its_memory_and_a_small_order_goes_first() {
    let (_h, mut warm, mut hub) =
        head_behind_an_idle_service("[services]\nevict_idle = false\n").await;
    hub.send(&order("small", req(4096, 1, 3600))).await;
    assert_eq!(hub.accepted().await.waiting, None);
    assert_eq!(warm.assign().await.job, JobId("small".into()));
}
