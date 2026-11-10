mod common;

use common::dir;
use lr_abstractions::store_contract::fixtures::{at, intake};
use lr_abstractions::{BrokerStore, PruneLimits, Transition};
use lr_core::{JobState, Timestamp};
use lr_store::SqliteBrokerStore;
use std::path::Path;

const DAY_MS: u64 = 86_400_000;

async fn run(store: &SqliteBrokerStore, job: &str, end: Option<u64>) -> lr_abstractions::JobKey {
    let i = intake("c1", job, "check", at(0));
    let key = i.job.key.clone();
    store.accept(i).await.unwrap();
    store.grant(&key, "r1").await.unwrap();
    store
        .transition(&key, Transition::to(JobState::Running, at(0)))
        .await
        .unwrap();
    store
        .append_log(&key, &[b'x'; 10], at(0), u64::MAX)
        .await
        .unwrap();
    if let Some(end) = end {
        store
            .transition(&key, Transition::to(JobState::Passed, at(end)))
            .await
            .unwrap();
    }
    key
}

fn mtime_ms(path: &Path) -> u64 {
    let t = std::fs::metadata(path).unwrap().modified().unwrap();
    t.duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

fn limits_at(now: u64) -> PruneLimits {
    PruneLimits {
        now: Timestamp(now),
        log_max_age_ms: u64::MAX,
        log_max_bytes: u64::MAX,
        row_max_age_ms: u64::MAX,
    }
}

#[tokio::test]
async fn prune_sweeps_old_orphan_log_files_but_not_fresh_ones_or_logged_ones() {
    let d = dir();
    let store = SqliteBrokerStore::open(&d.path().join("broker.db")).unwrap();
    let key = run(&store, "a", None).await;
    let stray = d.path().join("logs").join("c1").join("stray.log");
    std::fs::write(&stray, b"left over").unwrap();
    let written = mtime_ms(&stray);
    let report = store.prune(limits_at(written + 1000)).await.unwrap();
    assert_eq!(report.orphans_removed, 0, "inside the grace period");
    assert!(stray.exists());
    let report = store
        .prune(limits_at(written + DAY_MS + 1000))
        .await
        .unwrap();
    assert_eq!((report.orphans_removed, report.files_failed), (1, 0));
    assert!(!stray.exists());
    assert_eq!(store.read_log(&key).await.unwrap().len(), 10);
}

#[tokio::test]
async fn prune_reports_files_it_could_not_remove() {
    let d = dir();
    let store = SqliteBrokerStore::open(&d.path().join("broker.db")).unwrap();
    run(&store, "gone", Some(1)).await;
    let log = d.path().join("logs").join("c1").join("gone.log");
    std::fs::remove_file(&log).unwrap();
    std::fs::create_dir(&log).unwrap();
    std::fs::write(log.join("inside"), b"x").unwrap();
    let limits = PruneLimits {
        now: Timestamp(at(0).0 + 40 * DAY_MS),
        log_max_age_ms: 30 * DAY_MS,
        log_max_bytes: u64::MAX,
        row_max_age_ms: u64::MAX,
    };
    let report = store.prune(limits).await.unwrap();
    assert_eq!(report.logs_removed, 1);
    assert_eq!(report.files_failed, 1);
}
