mod common;

use common::faulty::FaultyStore;
use common::harness::*;
use lr_abstractions::Level;
use lr_proto::*;
use std::sync::atomic::Ordering;
use std::time::Duration;

const ALEX: &str = "local:alex";

fn unavailable(m: BrokerToHub) {
    match m {
        BrokerToHub::Error(e) => assert_eq!(e.code, ProtocolErrorCode::Unavailable),
        other => panic!("expected an unavailable error, got {other:?}"),
    }
}

#[tokio::test]
async fn store_failure_at_intake_is_answered_and_logged() {
    let h = Harness::start("");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    FaultyStore::set(&h.faults.fail_accept);
    hub.send(&order("j1", req(512, 1, 60))).await;
    unavailable(hub.hub().await);
    assert!(h.job_row(ALEX, "j1").await.is_none());
    assert!(h
        .log
        .records()
        .iter()
        .any(|r| r.level == Level::Error && r.fields.iter().any(|(_, v)| v == "j1")));
    h.faults.fail_accept.store(false, Ordering::SeqCst);
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
}

#[tokio::test]
async fn accepted_is_sent_only_after_the_order_is_committed() {
    let h = Harness::start("");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    FaultyStore::set(&h.faults.hold_accept);
    hub.send(&order("j1", req(512, 1, 60))).await;
    tokio::time::timeout(Duration::from_secs(5), h.faults.accept_entered.notified())
        .await
        .expect("the broker commits the order");
    hub.quiet().await;
    assert!(h.job_row(ALEX, "j1").await.is_none());
    h.faults.accept_release.notify_one();
    hub.accepted().await;
    assert!(h.job_row(ALEX, "j1").await.is_some());
}

#[tokio::test]
async fn seq_store_failure_at_intake_is_answered_and_logged() {
    let h = Harness::start("");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    FaultyStore::set(&h.faults.fail_accept_seq);
    hub.send(&order("j1", req(512, 1, 60))).await;
    unavailable(hub.hub().await);
    assert!(h.log.records().iter().any(|r| r.level == Level::Error));
}

#[tokio::test]
async fn late_message_for_a_job_not_on_the_runner_is_logged() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    r.send(&started("zz", 1)).await;
    r.acked(1).await;
    let rec = h
        .log
        .records()
        .into_iter()
        .find(|r| r.fields.iter().any(|(k, v)| k == "job" && v == "zz"))
        .expect("late message logged");
    assert!(rec.message.contains("late"), "{}", rec.message);
}

#[tokio::test]
async fn a_resent_order_whose_stored_spec_is_unreadable_is_queued_from_the_resent_order() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    FaultyStore::set(&h.faults.fail_accept_seq);
    hub.send(&order("j1", req(512, 1, 60))).await;
    unavailable(hub.hub().await);
    FaultyStore::clear(&h.faults.fail_accept_seq);
    FaultyStore::set(&h.faults.garble_spec);
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    let a = r.assign().await;
    assert_eq!(a.job, lr_core::JobId("j1".into()));
    assert_eq!(a.spec.requirements.mem_mb, 512);
}

#[tokio::test]
async fn a_resent_order_with_an_unreadable_stored_spec_that_no_longer_fits_is_rejected() {
    let h = Harness::start("");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    FaultyStore::set(&h.faults.fail_accept_seq);
    hub.send(&order("j1", req(512, 1, 60))).await;
    unavailable(hub.hub().await);
    FaultyStore::clear(&h.faults.fail_accept_seq);
    FaultyStore::set(&h.faults.garble_spec);
    hub.send(&order("j1", req(10_000_000, 1, 60))).await;
    let r = hub.rejected().await;
    assert_eq!(r.code.as_str(), "rejected_pool_memory");
}

#[tokio::test]
async fn repeated_order_at_seq_0_is_malformed() {
    let h = Harness::start("");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    FaultyStore::set(&h.faults.fail_accept_seq);
    hub.send(&order("j1", req(512, 1, 60))).await;
    unavailable(hub.hub().await);
    h.faults.fail_accept_seq.store(false, Ordering::SeqCst);
    let HubToBroker::Order(mut o) = order("j1", req(512, 1, 60)) else {
        unreachable!()
    };
    o.seq = 0;
    hub.send(&HubToBroker::Order(o)).await;
    match hub.hub().await {
        BrokerToHub::Error(e) => assert_eq!(e.code, ProtocolErrorCode::Malformed),
        other => panic!("expected malformed, got {other:?}"),
    }
}
