mod common;

use common::faulty::FaultyStore;
use common::harness::*;
use lr_abstractions::Level;
use lr_proto::*;
use std::sync::atomic::Ordering;

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
