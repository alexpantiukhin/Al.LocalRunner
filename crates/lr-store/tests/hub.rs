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

const HUB_V1: &str = "
CREATE TABLE broker_link (broker TEXT PRIMARY KEY, address TEXT NOT NULL,
    fingerprint TEXT NOT NULL, key_id TEXT NOT NULL, machine_id TEXT NOT NULL, last_zone TEXT);
CREATE TABLE active_order (job_id TEXT PRIMARY KEY, broker TEXT,
    last_seq INTEGER NOT NULL DEFAULT 0, label TEXT NOT NULL, os_user TEXT NOT NULL,
    submitter_pid INTEGER NOT NULL, submitter_start INTEGER, handle_path TEXT,
    detached INTEGER NOT NULL);
CREATE TABLE outbox (job_id TEXT NOT NULL REFERENCES active_order (job_id) ON DELETE CASCADE,
    seq INTEGER NOT NULL, kind TEXT NOT NULL, payload_json TEXT NOT NULL,
    created_at INTEGER NOT NULL, acked_at INTEGER, PRIMARY KEY (job_id, seq));
INSERT INTO active_order (job_id, broker, last_seq, label, os_user, submitter_pid, detached)
    VALUES ('j1', 'win', 3, 'agent', 'alex', 4242, 0);
PRAGMA application_id = 1280460850;
PRAGMA user_version = 1;
";

#[tokio::test]
async fn version_1_hub_db_is_migrated_keeping_active_orders() {
    let d = common::dir();
    let path = d.path().join("hub.db");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(HUB_V1)
        .unwrap();
    let store = SqliteHubStore::open_existing(&path).unwrap();
    let a = store.active(&JobId("j1".into())).await.unwrap().unwrap();
    assert_eq!(
        (a.broker.as_deref(), a.last_seq, a.spec_json),
        (Some("win"), 3, None)
    );
    assert_eq!(store.job_id_floor().await.unwrap(), 0);
    store.raise_job_id_floor(7).await.unwrap();
    assert_eq!(store.reopen().unwrap().job_id_floor().await.unwrap(), 7);
    let version: u32 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 2);
}
