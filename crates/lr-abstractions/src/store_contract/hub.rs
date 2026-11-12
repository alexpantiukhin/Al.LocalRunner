//! Contract cases of [`HubStore`].

use super::fixtures::at;
use crate::store::{ActiveOrder, BrokerLink, HubStore, OutboxBacklog, OutboxKind, StoreError};
use lr_core::{JobId, Zone};

fn id(s: &str) -> JobId {
    JobId(s.to_string())
}

async fn submitted(store: &dyn HubStore, job: &str, secs: u64) -> u64 {
    let order = ActiveOrder::new(id(job), "agent", "alex", 4242, Some(99));
    store
        .enqueue(order, OutboxKind::Order, format!("order {job}"), at(secs))
        .await
        .unwrap()
}

/// A submitted order is active and waits in the outbox until the broker's `Accepted`.
pub async fn order_waits_in_outbox_until_accepted(store: &dyn HubStore) {
    let seq = submitted(store, "j1", 1).await;
    assert_eq!(seq, 1);
    let active = store.active(&id("j1")).await.unwrap().unwrap();
    assert_eq!(active.submitter_pid, 4242);
    assert_eq!(active.submitter_start, Some(99));
    let pending = store.pending_outbox().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].kind, OutboxKind::Order);
    assert_eq!(pending[0].payload, "order j1");
    assert_eq!(store.ack_outbox(&id("j1"), seq).await.unwrap(), 1);
    assert!(store.pending_outbox().await.unwrap().is_empty());
    assert!(store.active(&id("j1")).await.unwrap().is_some());
}

/// The same job cannot be submitted twice.
pub async fn enqueue_of_active_job_refused(store: &dyn HubStore) {
    submitted(store, "j1", 1).await;
    let again = ActiveOrder::new(id("j1"), "agent", "alex", 1, None);
    assert!(matches!(
        store
            .enqueue(again, OutboxKind::Order, "x".into(), at(2))
            .await,
        Err(StoreError::Duplicate(_))
    ));
    assert_eq!(store.pending_outbox().await.unwrap().len(), 1);
}

/// Seq counts per job from 1, keeps growing after acks, and an ack is cumulative.
pub async fn outbox_seq_per_job_survives_ack_and_ack_is_cumulative(store: &dyn HubStore) {
    submitted(store, "j1", 1).await;
    submitted(store, "j2", 2).await;
    let cancel = |job: &'static str| async move {
        store
            .push_outbox(&id(job), OutboxKind::Cancel, "c".into(), at(3))
            .await
            .unwrap()
    };
    assert_eq!(cancel("j1").await, 2);
    assert_eq!(cancel("j1").await, 3);
    assert_eq!(cancel("j2").await, 2);
    assert_eq!(store.ack_outbox(&id("j1"), 2).await.unwrap(), 2);
    assert_eq!(cancel("j1").await, 4);
    let left: Vec<(String, u64)> = store
        .pending_outbox()
        .await
        .unwrap()
        .iter()
        .map(|e| (e.job.0.clone(), e.seq))
        .collect();
    assert_eq!(
        left,
        vec![
            ("j2".into(), 1),
            ("j2".into(), 2),
            ("j1".into(), 3),
            ("j1".into(), 4)
        ]
    );
}

/// A cancel before `Accepted` discards the order entry and still goes out as a cancel
/// (spec 3.1).
pub async fn cancel_before_accepted_discards_order_entry(store: &dyn HubStore) {
    submitted(store, "j1", 1).await;
    assert_eq!(store.discard_outbox(&id("j1")).await.unwrap(), 1);
    let seq = store
        .push_outbox(&id("j1"), OutboxKind::Cancel, "c".into(), at(2))
        .await
        .unwrap();
    assert_eq!(seq, 2);
    let pending = store.pending_outbox().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].kind, OutboxKind::Cancel);
}

/// Outbox entries belong to an active order.
pub async fn outbox_push_needs_active_order(store: &dyn HubStore) {
    assert!(matches!(
        store
            .push_outbox(&id("ghost"), OutboxKind::Cancel, "c".into(), at(1))
            .await,
        Err(StoreError::NotFound(_))
    ));
}

/// A message seq from the broker seen before is a repeat; `last_seq` follows the greatest.
pub async fn received_seq_repeat_dropped_and_last_seq_kept(store: &dyn HubStore) {
    submitted(store, "j1", 1).await;
    assert!(store.accept_seq(&id("j1"), 1).await.unwrap());
    assert!(!store.accept_seq(&id("j1"), 1).await.unwrap());
    assert!(store.accept_seq(&id("j1"), 2).await.unwrap());
    assert_eq!(store.active(&id("j1")).await.unwrap().unwrap().last_seq, 2);
}

/// A finished job leaves no active order and no outbox entries.
pub async fn finish_removes_active_order_and_its_outbox(store: &dyn HubStore) {
    submitted(store, "j1", 1).await;
    submitted(store, "j2", 1).await;
    store.finish(&id("j1")).await.unwrap();
    assert!(store.active(&id("j1")).await.unwrap().is_none());
    let pending = store.pending_outbox().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].job, id("j2"));
    assert_eq!(store.active_orders().await.unwrap().len(), 1);
    assert!(matches!(
        store.finish(&id("j1")).await,
        Err(StoreError::NotFound(_))
    ));
}

/// The broker of an active order is set on choice and cleared for a new choice.
pub async fn broker_of_active_order_set_and_cleared(store: &dyn HubStore) {
    submitted(store, "j1", 1).await;
    store.set_broker(&id("j1"), Some("win-host")).await.unwrap();
    let a = store.active(&id("j1")).await.unwrap().unwrap();
    assert_eq!(a.broker.as_deref(), Some("win-host"));
    store.set_broker(&id("j1"), None).await.unwrap();
    assert_eq!(store.active(&id("j1")).await.unwrap().unwrap().broker, None);
}

/// Re-brokering starts both streams of the job over: broker and `last_seq` cleared, old
/// entries gone, the new order is seq 1 (spec 3.1, 3.9).
pub async fn rebroker_restarts_both_streams_at_one(store: &dyn HubStore) {
    submitted(store, "j1", 1).await;
    submitted(store, "j2", 1).await;
    store.set_broker(&id("j1"), Some("a")).await.unwrap();
    store.ack_outbox(&id("j1"), 1).await.unwrap();
    store
        .push_outbox(&id("j1"), OutboxKind::Cancel, "c".into(), at(2))
        .await
        .unwrap();
    assert!(store.accept_seq(&id("j1"), 1).await.unwrap());
    let seq = store
        .rebroker(&id("j1"), OutboxKind::Order, "again".into(), at(3))
        .await
        .unwrap();
    assert_eq!(seq, 1);
    let a = store.active(&id("j1")).await.unwrap().unwrap();
    assert_eq!((a.broker, a.last_seq), (None, 0));
    let mine: Vec<(u64, String)> = store
        .pending_outbox()
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.job == id("j1"))
        .map(|e| (e.seq, e.payload))
        .collect();
    assert_eq!(mine, vec![(1, "again".to_string())]);
    assert_eq!(
        store
            .push_outbox(&id("j1"), OutboxKind::Cancel, "c2".into(), at(4))
            .await
            .unwrap(),
        2
    );
    assert!(store.accept_seq(&id("j1"), 1).await.unwrap());
    assert_eq!(store.pending_outbox().await.unwrap().len(), 3);
    assert!(matches!(
        store
            .rebroker(&id("ghost"), OutboxKind::Order, "x".into(), at(5))
            .await,
        Err(StoreError::NotFound(_))
    ));
}

/// The broker list keeps address, fingerprint and the last zone.
pub async fn broker_list_upsert_zone_and_remove(store: &dyn HubStore) {
    let link = BrokerLink {
        name: "win-host".into(),
        address: "192.168.1.2:7000".into(),
        fingerprint: "fp".into(),
        key_id: "k".into(),
        machine_id: "m-7f3a".into(),
        last_zone: None,
    };
    store.upsert_broker(link.clone()).await.unwrap();
    store.set_zone("win-host", Zone::Lan).await.unwrap();
    let stored = store.brokers().await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].last_zone, Some(Zone::Lan));
    store
        .upsert_broker(BrokerLink {
            address: "10.0.0.2:7000".into(),
            ..link
        })
        .await
        .unwrap();
    assert_eq!(store.brokers().await.unwrap()[0].address, "10.0.0.2:7000");
    store.remove_broker("win-host").await.unwrap();
    assert!(store.brokers().await.unwrap().is_empty());
    assert!(matches!(
        store.set_zone("win-host", Zone::Lan).await,
        Err(StoreError::NotFound(_))
    ));
}

/// The backlog counts unacknowledged entries and the oldest of them (spec 12.2).
pub async fn outbox_backlog_counts_pending_and_oldest(store: &dyn HubStore) {
    assert_eq!(
        store.outbox_backlog().await.unwrap(),
        OutboxBacklog::default()
    );
    let first = submitted(store, "j1", 3).await;
    submitted(store, "j2", 8).await;
    let b = store.outbox_backlog().await.unwrap();
    assert_eq!((b.pending, b.oldest), (2, Some(at(3))));
    store.ack_outbox(&id("j1"), first).await.unwrap();
    let b = store.outbox_backlog().await.unwrap();
    assert_eq!((b.pending, b.oldest), (1, Some(at(8))));
}
