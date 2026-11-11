mod common;

use common::*;
use lr_proto::{CliReply, ReasonCode, RejectReason};
use std::time::Duration;

const HOUR: u64 = 3600;

#[tokio::test]
async fn outbox_expiry_is_the_earlier_of_sender_death_and_ttl() {
    let mut rig = Rig::new();
    rig.local = true;
    rig.policy.outbox.ttl_hours = 1;
    rig.start();
    let job = rig.cli().await.submit(submit("unit", None)).await;
    rig.clock.advance(Duration::from_secs(HOUR / 2));
    settle().await;
    assert_eq!(
        rig.pending().await.len(),
        1,
        "alive sender, ttl not reached"
    );
    rig.inspector.remove(SENDER_PID);
    rig.step(7).await;
    assert!(
        rig.active(&job).await.is_none(),
        "the sender died first: the order goes half an hour before ttl_hours"
    );
    assert!(rig.pending().await.is_empty());
}

#[tokio::test]
async fn detached_order_ignores_sender_death_and_expires_at_ttl() {
    let mut rig = Rig::new();
    rig.local = true;
    rig.policy.outbox.ttl_hours = 1;
    rig.start();
    let mut req = submit("unit", None);
    req.detach = true;
    let job = rig.cli().await.submit(req).await;
    rig.inspector.remove(SENDER_PID);
    rig.step(20).await;
    rig.clock.advance(Duration::from_secs(HOUR - 21));
    settle().await;
    assert!(rig.active(&job).await.is_some(), "ttl not reached yet");
    rig.step(1).await;
    assert!(rig.active(&job).await.is_none(), "ttl_hours came first");
    assert!(rig.pending().await.is_empty());
}

#[tokio::test]
async fn expired_order_is_rejected_with_the_broker_never_came_reason() {
    let mut rig = Rig::new();
    rig.local = true;
    rig.policy.outbox.ttl_hours = 2;
    rig.start();
    let cli = rig.cli().await;
    let mut req = submit("unit", None);
    req.wait = true;
    let job = cli.submit(req).await;
    rig.clock.advance(Duration::from_secs(2 * HOUR));
    settle().await;
    rig.step(1).await;
    match cli.reply().await {
        CliReply::Rejected { code, detail } => {
            assert_eq!(code, ReasonCode::Rejected(RejectReason::OutboxExpired));
            assert_eq!(detail, "брокер не появился за срок: 2 ч");
        }
        other => panic!("{other:?}"),
    }
    let log = String::from_utf8(rig.store.file(&log_path(&job)).unwrap()).unwrap();
    assert!(log.contains("брокер не появился за срок: 2 ч"), "{log}");
}
