mod common;

use common::broker::Script;
use common::*;
use lr_abstractions::fakes::ManualClock;
use lr_abstractions::{ActiveOrder, HubStore, OutboxKind};
use lr_core::{JobId, JobState, Timestamp, Zone};
use lr_proto::{HubCancel, HubToBroker, ReasonCode, RejectReason};

fn log_of(rig: &Rig, job: &JobId) -> String {
    String::from_utf8(rig.store.file(&log_path(job)).unwrap_or_default()).unwrap()
}

#[tokio::test]
async fn after_restart_a_returned_order_goes_to_another_broker_at_seq_one() {
    let mut rig = Rig::new();
    rig.policy.brokers.order = vec!["lan-a".into(), "lan-b".into()];
    rig.join("lan-a").await;
    rig.join("lan-b").await;
    let a = rig.broker("lan-a", Zone::Lan, Script::linux(Zone::Lan));
    let b = rig.broker("lan-b", Zone::Lan, Script::linux(Zone::Lan));
    let hub = rig.start();
    eventually("a connected", || a.resumed()).await;
    eventually("b connected", || b.resumed()).await;
    settle().await;
    let job = rig
        .cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    eventually("accepted by a", || a.orders().len() == 1).await;
    settle().await;
    assert!(rig.pending().await.is_empty());
    let _hub = rig.restart(hub).await;
    eventually("a reconnected", || a.hellos() == 2).await;
    settle().await;
    a.returned(&job, ReasonCode::RunnerRemoved).await;
    eventually("replacement at b", || b.orders().len() == 1).await;
    let order = &b.orders()[0];
    assert_eq!((order.job.clone(), order.seq), (job.clone(), 1));
    assert_eq!(order.spec.enqueued_at.0, START);
    assert_eq!(order.spec.profile, "remote");
    settle().await;
    assert_eq!(
        rig.active(&job).await.unwrap().broker.as_deref(),
        Some("lan-b")
    );
}

#[tokio::test]
async fn restored_same_machine_order_returned_ends_with_the_up_hint() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    let hub = rig.start();
    let job = rig.cli().await.submit(submit("unit", None)).await;
    eventually("accepted", || local.orders().len() == 1).await;
    settle().await;
    let _hub = rig.restart(hub).await;
    eventually("reconnected", || local.hellos() == 2).await;
    settle().await;
    local
        .returned(&job, ReasonCode::Rejected(RejectReason::NoRunnerOnMachine))
        .await;
    settle().await;
    assert!(rig.active(&job).await.is_none());
    let log = log_of(&rig, &job);
    assert!(log.contains("rejected_no_runner_on_machine"), "{log}");
    assert!(log.contains("localrunner up"), "{log}");
    assert_eq!(local.orders().len(), 1);
}

#[tokio::test]
async fn job_ids_stay_unique_across_a_restart_with_the_clock_gone_back() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    let hub = rig.start();
    let first = rig.cli().await.submit(submit("unit", None)).await;
    eventually("accepted", || local.orders().len() == 1).await;
    settle().await;
    local.result(&first, JobState::Passed, Some(0)).await;
    settle().await;
    assert!(rig.active(&first).await.is_none());
    rig.step(10).await;
    rig.hub_clock = Some(ManualClock::new(Timestamp(START)));
    let _hub = rig.restart(hub).await;
    let second = rig.cli().await.submit(submit("unit", None)).await;
    assert_ne!(first, second);
}

fn restored(job: &str, broker: Option<&str>) -> ActiveOrder {
    let mut o = ActiveOrder::new(
        JobId(job.into()),
        "agent",
        "alex",
        SENDER_PID,
        Some(SENDER_START),
    );
    o.broker = broker.map(str::to_string);
    o
}

#[tokio::test]
async fn restored_pending_cancel_is_not_cancelled_twice() {
    let mut rig = Rig::new();
    let job = JobId("restored".into());
    let at = Timestamp(START);
    let store = rig.store.clone();
    store
        .enqueue(
            restored("restored", Some("local")),
            OutboxKind::Order,
            "{}".into(),
            at,
        )
        .await
        .unwrap();
    store.ack_outbox(&job, 1).await.unwrap();
    let cancel = HubToBroker::Cancel(HubCancel {
        job: job.clone(),
        seq: 0,
        by: Box::new(identity("agent")),
        force: false,
        code: ReasonCode::CancelledByUser,
        reason: None,
        client: None,
    });
    let payload = serde_json::to_string(&cancel).unwrap();
    store
        .push_outbox(&job, OutboxKind::Cancel, payload, at)
        .await
        .unwrap();
    let local = rig.local_broker(Script::local_runner());
    rig.inspector.remove(SENDER_PID);
    rig.start();
    eventually("pending cancel resent", || local.cancels().len() == 1).await;
    rig.step(10).await;
    let seqs: Vec<u64> = local.cancels().iter().map(|c| c.seq).collect();
    assert_eq!(seqs, vec![2], "the restored cancel only");
}

#[tokio::test]
async fn restored_order_with_an_unreadable_payload_ends_at_start() {
    let mut rig = Rig::new();
    rig.local = true;
    rig.store
        .enqueue(
            restored("broken", None),
            OutboxKind::Order,
            "{}".into(),
            Timestamp(START),
        )
        .await
        .unwrap();
    rig.start();
    settle().await;
    assert!(rig.active(&JobId("broken".into())).await.is_none());
    assert!(rig.pending().await.is_empty());
    assert!(rig.log_text().contains("unreadable"), "{}", rig.log_text());
}

#[tokio::test]
async fn corrupt_outbox_entry_ends_its_job_with_an_error() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let job = rig.cli().await.submit(submit("unit", None)).await;
    eventually("accepted", || local.orders().len() == 1).await;
    settle().await;
    rig.store
        .push_outbox(&job, OutboxKind::Cancel, "garbage".into(), Timestamp(START))
        .await
        .unwrap();
    local.disconnect().await;
    rig.step(2).await;
    eventually("reconnected", || local.hellos() == 2).await;
    settle().await;
    assert!(rig.active(&job).await.is_none());
    let log = log_of(&rig, &job);
    assert!(log.contains("rejected_order_unreadable"), "{log}");
}
