//! Contract cases of [`RunnerStore`].

use super::fixtures::at;
use crate::store::{
    CacheEntry, CacheKey, OutboxKind, RunnerStore, ServiceRow, ServiceState, StoreError,
};
use lr_core::JobId;

fn id(s: &str) -> JobId {
    JobId(s.to_string())
}

/// Output and results wait in the outbox with seq per job until acked, in order.
pub async fn runner_outbox_keeps_order_until_acked(store: &dyn RunnerStore) {
    for (kind, t) in [
        (OutboxKind::Started, 1),
        (OutboxKind::Chunk, 2),
        (OutboxKind::JobResult, 3),
    ] {
        store
            .push_outbox(&id("j1"), kind, format!("{kind:?}"), at(t))
            .await
            .unwrap();
    }
    let pending = store.pending_outbox().await.unwrap();
    let seqs: Vec<u64> = pending.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3]);
    assert_eq!(pending[2].kind, OutboxKind::JobResult);
    assert_eq!(store.ack_outbox(&id("j1"), 2).await.unwrap(), 2);
    let left = store.pending_outbox().await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].seq, 3);
    let next = store
        .push_outbox(&id("j1"), OutboxKind::Chunk, "x".into(), at(4))
        .await
        .unwrap();
    assert_eq!(next, 4);
}

/// Entries of one job, or all of them when the broker removed the runner, can be discarded.
pub async fn runner_outbox_discard_and_clear(store: &dyn RunnerStore) {
    for job in ["j1", "j2"] {
        store
            .push_outbox(&id(job), OutboxKind::Chunk, "x".into(), at(1))
            .await
            .unwrap();
    }
    assert_eq!(store.discard_outbox(&id("j1")).await.unwrap(), 1);
    assert_eq!(store.pending_outbox().await.unwrap()[0].job, id("j2"));
    assert_eq!(store.clear_outbox().await.unwrap(), 1);
    assert!(store.pending_outbox().await.unwrap().is_empty());
}

fn service(name: &str, pid: u32, state: ServiceState) -> ServiceRow {
    ServiceRow {
        name: name.to_string(),
        pid,
        pid_start: Some(77),
        state,
        started_at: at(1),
        expires_at: at(14_401),
        mem_mb: 2048,
    }
}

/// The service registry keeps pid, start fingerprint and expiry for the orphan search.
pub async fn service_registry_upsert_list_remove(store: &dyn RunnerStore) {
    store
        .upsert_service(service("build-daemon", 10, ServiceState::Starting))
        .await
        .unwrap();
    store
        .upsert_service(service("build-daemon", 10, ServiceState::Idle))
        .await
        .unwrap();
    store
        .upsert_service(service("emulator", 11, ServiceState::Busy))
        .await
        .unwrap();
    let services = store.services().await.unwrap();
    assert_eq!(services.len(), 2);
    assert_eq!(services[0].name, "build-daemon");
    assert_eq!(services[0].state, ServiceState::Idle);
    assert_eq!(services[0].pid_start, Some(77));
    store.remove_service("build-daemon").await.unwrap();
    assert_eq!(store.services().await.unwrap().len(), 1);
    assert!(matches!(
        store.remove_service("build-daemon").await,
        Err(StoreError::NotFound(_))
    ));
}

/// The cache index is keyed by client, project and name and keeps size and last use.
pub async fn cache_index_keyed_by_client_project_and_name(store: &dyn RunnerStore) {
    let key = |client: &str| CacheKey {
        client: client.to_string(),
        project: "owner/repo".into(),
        name: "cargo".into(),
    };
    let entry = |client: &str, bytes: u64, t: u64| CacheEntry {
        key: key(client),
        path: format!("/cache/{client}/cargo"),
        bytes,
        last_used: at(t),
    };
    store.upsert_cache(entry("c1", 10, 1)).await.unwrap();
    store.upsert_cache(entry("c2", 20, 2)).await.unwrap();
    store.upsert_cache(entry("c1", 30, 3)).await.unwrap();
    let entries = store.cache_entries().await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], entry("c1", 30, 3));
    store.remove_cache(&key("c1")).await.unwrap();
    assert_eq!(
        store.cache_entries().await.unwrap(),
        vec![entry("c2", 20, 2)]
    );
    assert!(matches!(
        store.remove_cache(&key("c1")).await,
        Err(StoreError::NotFound(_))
    ));
}
