mod common;

use common::hub;
use lr_abstractions::{ActiveOrder, HubStore, OutboxKind};
use lr_core::{JobId, Timestamp};
use lr_store::SqliteHubStore;

lr_abstractions::hub_store_contract!(
    make: hub(),
    reopen: |s: SqliteHubStore| s.reopen().unwrap(),
);

#[tokio::test]
async fn log_copy_is_appended_in_the_submitter_directory() {
    let store = hub();
    let d = common::dir();
    let path = d
        .path()
        .join("repo")
        .join("temp")
        .join("runner")
        .join("j1.log");
    store.append_log_copy(&path, b"one\n").await.unwrap();
    store.append_log_copy(&path, b"two\n").await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"one\ntwo\n");
}

#[tokio::test]
async fn survives_reopen_with_active_orders_brokers_and_acked_state() {
    let store = hub();
    let id = JobId("j1".into());
    let mut order = ActiveOrder::new(id.clone(), "agent", "alex", 4242, Some(u64::MAX - 5));
    order.handle_path = Some("temp/runner/j1.json".into());
    order.detached = true;
    store
        .enqueue(order.clone(), OutboxKind::Order, "o".into(), Timestamp(5))
        .await
        .unwrap();
    store.set_broker(&id, Some("win")).await.unwrap();
    let store = store.reopen().unwrap();
    order.broker = Some("win".into());
    assert_eq!(store.active(&id).await.unwrap(), Some(order));
    assert_eq!(store.pending_outbox().await.unwrap()[0].payload, "o");
}
