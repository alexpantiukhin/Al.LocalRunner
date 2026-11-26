mod common;

use common::harness::*;
use lr_proto::*;

fn spec_of(msg: HubToBroker) -> Box<OrderSpec> {
    let HubToBroker::Order(o) = msg else {
        panic!("an order")
    };
    o.spec
}

async fn start_now(hub: &mut Conn, req: u64, probe: Option<Box<OrderSpec>>) -> Option<bool> {
    hub.send(&HubToBroker::CapabilitiesQuery(CapabilitiesQuery {
        req,
        probe,
    }))
    .await;
    loop {
        if let BrokerToHub::CapabilitiesReply(c) = hub.hub().await {
            assert_eq!(c.req, req);
            return c.start_now;
        }
    }
}

#[tokio::test]
async fn an_idle_runner_would_start_the_probed_order_now() {
    let h = Harness::start("");
    let _r = h.local_runner("alex", "lr1", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("local:lr1", true).await;
    let probe = spec_of(order("probe", req(512, 1, 60)));
    assert_eq!(start_now(&mut hub, 1, Some(probe)).await, Some(true));
}

#[tokio::test]
async fn a_busy_runner_would_not_start_the_probed_order_now() {
    let h = Harness::start("");
    let mut r = h.local_runner("alex", "lr1", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("local:lr1", true).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    assert_eq!(r.assign().await.job.0, "j1");
    let probe = spec_of(order("probe", req(512, 1, 60)));
    assert_eq!(start_now(&mut hub, 2, Some(probe)).await, Some(false));
}

#[tokio::test]
async fn a_query_without_an_order_has_no_start_now() {
    let h = Harness::start("");
    let _r = h.local_runner("alex", "lr1", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("local:lr1", true).await;
    assert_eq!(start_now(&mut hub, 3, None).await, None);
}

#[tokio::test]
async fn an_order_that_does_not_fit_the_pool_would_not_start_now() {
    let h = Harness::start("");
    let _r = h.local_runner("alex", "lr1", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("local:lr1", true).await;
    let probe = spec_of(order("probe", req(1_000_000, 1, 60)));
    assert_eq!(start_now(&mut hub, 4, Some(probe)).await, Some(false));
}

#[tokio::test]
async fn probing_leaves_the_queue_unchanged() {
    let h = Harness::start("");
    let mut r = h.local_runner("alex", "lr1", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("local:lr1", true).await;
    let probe = spec_of(order("probe", req(512, 1, 60)));
    assert_eq!(start_now(&mut hub, 5, Some(probe)).await, Some(true));
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    assert_eq!(r.assign().await.job.0, "j1");
}
