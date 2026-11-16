mod common;

use common::harness::*;
use lr_core::JobId;
use lr_proto::*;

#[tokio::test]
async fn a_work_tree_order_with_no_runner_at_all_keeps_the_start_it_hint() {
    let h = Harness::start("");
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("tree", same_machine_req())).await;
    let refused = hub.rejected().await;
    assert_eq!(
        refused.code,
        ReasonCode::Rejected(RejectReason::NoRunnerOnMachine)
    );
    assert_eq!(
        refused.detail,
        "нет раннера на вашей установке, запустите его: up"
    );
}

#[tokio::test]
async fn a_container_runner_never_gets_a_work_tree_order_and_takes_commit_orders() {
    let h = Harness::start("");
    let mut container = h.member_runner("box", "172.17.0.2", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("tree", same_machine_req())).await;
    let refused = hub.rejected().await;
    assert_eq!(
        refused.code,
        ReasonCode::Rejected(RejectReason::NoRunnerOnMachine)
    );
    assert_eq!(
        refused.detail,
        "раннер box подключён по ключу: он берёт только заказы по коммиту; заказу по рабочему дереву нужен раннер, подключённый через локальный канал брокера на машине клиента"
    );
    container.quiet().await;
    hub.send(&order("commit", req(512, 1, 60))).await;
    hub.accepted().await;
    let a = container.assign().await;
    assert_eq!(a.job, JobId("commit".into()));
    assert_eq!(a.spec.requirements.same_machine, None);
}
