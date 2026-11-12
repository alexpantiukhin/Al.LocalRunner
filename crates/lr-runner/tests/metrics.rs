mod common;

use common::*;
use lr_abstractions::metric::{
    CACHE_EVICTED_TOTAL, CACHE_USED_MB, OUTBOX_OLDEST_AGE_SECONDS, OUTBOX_PENDING, OUTBOX_SAMPLE_S,
    REASON, ROLE, RUNNER,
};
use lr_abstractions::{CacheEntry, CacheKey, OutboxKind, RunnerStore};
use lr_core::{JobId, Timestamp};
use lr_runner::Retention;
use std::path::Path;
use std::time::Duration;

const DAY: u64 = 86_400_000;
const MB: u64 = 1024 * 1024;
const ME: &[(&str, &str)] = &[(RUNNER, "runner-a")];

async fn eventually(what: &str, check: impl Fn() -> bool) {
    for _ in 0..30_000 {
        if check() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("{what} never happened");
}

fn entry(client: &str, name: &str, bytes: u64, last_used: u64) -> CacheEntry {
    CacheEntry {
        key: CacheKey {
            client: client.into(),
            project: "owner/secret-project".into(),
            name: name.into(),
        },
        path: format!("/state/runner-a/cache/{client}/{name}"),
        bytes,
        last_used: Timestamp(last_used),
    }
}

#[tokio::test]
async fn cache_sweep_counts_evictions_by_reason_and_reports_the_fill() {
    let mut rig = Rig::new();
    rig.settings.cache = Retention {
        max_size_gb: Some(1),
        retention_days: Some(14),
    };
    for e in [
        entry("alice", "stale", 5, START - 15 * DAY),
        entry("alice", "older", 600 * MB, START - 2 * DAY),
        entry("bob", "newer", 600 * MB, START - DAY),
    ] {
        rig.files.put(&Path::new(&e.path).join("f"), b"x");
        rig.store.upsert_cache(e).await.unwrap();
    }
    let (_c, _h) = rig.start();
    let _b = Broker::accept(&rig).await;
    let m = &rig.meter;
    let by = |reason| [(RUNNER, "runner-a"), (REASON, reason)];
    assert_eq!(m.counter(CACHE_EVICTED_TOTAL, &by("age")), 1);
    assert_eq!(m.counter(CACHE_EVICTED_TOTAL, &by("size")), 1);
    assert_eq!(m.gauge(CACHE_USED_MB, ME), Some(600.0));
    assert_eq!(rig.store.cache_entries().await.unwrap().len(), 1);
    for (k, v) in m.label_pairs() {
        assert!(["runner", "reason", "role"].contains(&k.as_str()), "{k}");
        for word in [
            "alice", "bob", "secret", "/state", "stale", "older", "newer",
        ] {
            assert!(!v.contains(word), "{k}={v} carries {word}");
        }
    }
}

#[tokio::test]
async fn workspace_mirrors_are_not_counted_as_cache_evictions() {
    let mut rig = Rig::new();
    rig.settings.workspace = Retention {
        max_size_gb: None,
        retention_days: Some(1),
    };
    let ws = entry(lr_runner::WORKSPACE_CLIENT, "mirror", 5, START - 3 * DAY);
    rig.files.put(&Path::new(&ws.path).join("f"), b"x");
    rig.store.upsert_cache(ws).await.unwrap();
    let (_c, _h) = rig.start();
    let _b = Broker::accept(&rig).await;
    assert!(rig.store.cache_entries().await.unwrap().is_empty());
    let m = &rig.meter;
    assert_eq!(
        m.counter(
            CACHE_EVICTED_TOTAL,
            &[(RUNNER, "runner-a"), (REASON, "age")]
        ),
        0
    );
    assert_eq!(m.gauge(CACHE_USED_MB, ME), Some(0.0));
}

#[tokio::test]
async fn outbox_is_sampled_at_start_and_every_period() {
    let rig = Rig::new();
    rig.store
        .push_outbox(
            &JobId("job-1".into()),
            OutboxKind::JobResult,
            "{}".into(),
            Timestamp(START - 30_000),
        )
        .await
        .unwrap();
    let (_c, _h) = rig.start();
    let m = rig.meter.clone();
    let role = [(ROLE, "runner")];
    eventually("first sample", || {
        m.gauge(OUTBOX_PENDING, &role) == Some(1.0)
    })
    .await;
    assert_eq!(m.gauge(OUTBOX_OLDEST_AGE_SECONDS, &role), Some(30.0));
    rig.advance(Duration::from_secs(OUTBOX_SAMPLE_S)).await;
    let expected = (30 + OUTBOX_SAMPLE_S) as f64;
    eventually("second sample", || {
        m.gauge(OUTBOX_OLDEST_AGE_SECONDS, &role) == Some(expected)
    })
    .await;
}
