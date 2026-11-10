mod common;

use common::runner;
use lr_abstractions::{CacheEntry, CacheKey, OutboxKind, RunnerStore, ServiceRow, ServiceState};
use lr_core::{JobId, Timestamp};
use lr_store::SqliteRunnerStore;

lr_abstractions::runner_store_contract!(
    make: runner(),
    reopen: |s: SqliteRunnerStore| s.reopen().unwrap(),
);

#[tokio::test]
async fn survives_reopen_with_services_and_cache_index() {
    let store = runner();
    let service = ServiceRow {
        name: "emulator".into(),
        pid: 77,
        pid_start: Some(u64::MAX - 1),
        state: ServiceState::Busy,
        started_at: Timestamp(10),
        expires_at: Timestamp(20),
        mem_mb: 2048,
    };
    let cache = CacheEntry {
        key: CacheKey {
            client: "c".into(),
            project: "p".into(),
            name: "cargo".into(),
        },
        path: "/cache/c/p/cargo".into(),
        bytes: u64::MAX / 2,
        last_used: Timestamp(30),
    };
    store.upsert_service(service.clone()).await.unwrap();
    store.upsert_cache(cache.clone()).await.unwrap();
    let store = store.reopen().unwrap();
    assert_eq!(store.services().await.unwrap(), vec![service]);
    assert_eq!(store.cache_entries().await.unwrap(), vec![cache]);
}

#[tokio::test]
async fn clear_and_discard_keep_the_seq_counter_so_a_seq_is_never_reused() {
    let store = runner();
    let job = JobId("j1".into());
    for _ in 0..2 {
        store
            .push_outbox(&job, OutboxKind::Chunk, "x".into(), Timestamp(1))
            .await
            .unwrap();
    }
    assert_eq!(store.clear_outbox().await.unwrap(), 2);
    let store = store.reopen().unwrap();
    let next = store
        .push_outbox(&job, OutboxKind::Chunk, "x".into(), Timestamp(2))
        .await
        .unwrap();
    assert_eq!(next, 3);
    assert_eq!(store.discard_outbox(&job).await.unwrap(), 1);
    let next = store
        .push_outbox(&job, OutboxKind::Chunk, "x".into(), Timestamp(3))
        .await
        .unwrap();
    assert_eq!(next, 4);
}
