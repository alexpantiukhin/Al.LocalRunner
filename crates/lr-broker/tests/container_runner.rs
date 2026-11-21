mod common;

use common::harness::*;
use lr_core::JobId;
use lr_proto::*;

#[tokio::test]
async fn a_work_tree_order_with_no_runner_at_all_keeps_the_start_it_hint() {
    let h = Harness::start("");
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("tree", local_only_req())).await;
    let refused = hub.rejected().await;
    assert_eq!(
        refused.code,
        ReasonCode::Rejected(RejectReason::NoRunnerOnMachine)
    );
    assert_eq!(
        refused.detail,
        "no runner on your installation, start it: up"
    );
}

#[tokio::test]
async fn a_container_runner_is_not_in_the_local_zone_and_takes_orders_of_its_own_zone() {
    let h = Harness::start("");
    let mut container = h.member_runner("box", "172.17.0.2", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("tree", local_only_req())).await;
    let refused = hub.rejected().await;
    assert_eq!(
        refused.code,
        ReasonCode::Rejected(RejectReason::NoRunnerOnMachine)
    );
    assert_eq!(
        refused.detail,
        "no runner on your installation, start it: up"
    );
    container.quiet().await;
    hub.send(&order("own", req(512, 1, 60))).await;
    hub.accepted().await;
    let a = container.assign().await;
    assert_eq!(a.job, JobId("own".into()));
    assert!(!a.spec.requirements.local_only());
}
