mod common;

use common::dir;
use lr_abstractions::store_contract::fixtures::{at, intake};
use lr_abstractions::{BrokerStore, JobKey, PruneLimits, StepUpdate, Transition};
use lr_core::{JobState, StepState, Timestamp};
use lr_store::SqliteBrokerStore;
use rusqlite::Connection;
use std::path::Path;

const DAY_MS: u64 = 86_400_000;

async fn run(
    store: &SqliteBrokerStore,
    job: &str,
    queued: u64,
    end: Option<u64>,
    log_bytes: usize,
) -> JobKey {
    let i = intake("c1", job, "check", at(queued));
    let key = i.job.key.clone();
    store.accept(i).await.unwrap();
    store.grant(&key, "r1").await.unwrap();
    store
        .transition(&key, Transition::to(JobState::Running, at(queued)))
        .await
        .unwrap();
    store
        .update_step(
            &key,
            StepUpdate {
                ord: 0,
                state: StepState::Passed,
                exit_code: Some(0),
                started_at: Some(at(queued)),
                finished_at: Some(at(queued + 1)),
            },
        )
        .await
        .unwrap();
    store
        .append_log(&key, &vec![b'x'; log_bytes], at(queued), u64::MAX)
        .await
        .unwrap();
    if let Some(end) = end {
        let mut done = Transition::to(JobState::Passed, at(end));
        done.exit_code = Some(0);
        store.transition(&key, done).await.unwrap();
    }
    key
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |r| {
        r.get(0)
    })
    .unwrap()
}

fn log_file(dir: &Path, rel: &str) -> bool {
    dir.join(rel).exists()
}

#[tokio::test]
async fn prune_by_age_and_size_skips_active_jobs() {
    let d = dir();
    let store = SqliteBrokerStore::open(&d.path().join("broker.db")).unwrap();
    let ancient = run(&store, "ancient", 0, Some(1), 100).await;
    let old = run(&store, "old", 10, Some(11), 100).await;
    let mid = run(&store, "mid", 20, Some(21), 400_000).await;
    let new = run(&store, "new", 30, Some(31), 400_000).await;
    let live = run(&store, "live", 0, None, 500_000).await;
    let paths = |keys: &[&JobKey]| -> Vec<String> {
        keys.iter()
            .map(|k| format!("logs/{}/{}.log", k.client, k.job))
            .collect()
    };
    for p in paths(&[&ancient, &old, &mid, &new, &live]) {
        assert!(log_file(d.path(), &p), "{p}");
    }

    let by_age = PruneLimits {
        now: Timestamp(at(0).0 + 30 * DAY_MS + 15_000),
        log_max_age_ms: 30 * DAY_MS,
        log_max_bytes: u64::MAX,
        row_max_age_ms: 365 * DAY_MS,
    };
    let report = store.prune(by_age).await.unwrap();
    assert_eq!((report.logs_removed, report.rows_removed), (2, 0));
    assert_eq!(report.bytes_freed, 200);
    assert!(store.job(&ancient).await.unwrap().unwrap().log.is_none());
    assert!(store.job(&old).await.unwrap().unwrap().log.is_none());
    assert!(!log_file(d.path(), "logs/c1/ancient.log"));
    assert!(log_file(d.path(), "logs/c1/mid.log"));
    assert!(log_file(d.path(), "logs/c1/live.log"));

    let by_size = PruneLimits {
        now: Timestamp(at(0).0 + 30 * DAY_MS + 15_000),
        log_max_age_ms: 365 * DAY_MS,
        log_max_bytes: 1_000_000,
        row_max_age_ms: 365 * DAY_MS,
    };
    let report = store.prune(by_size).await.unwrap();
    assert_eq!(report.logs_removed, 1, "only the oldest finished log goes");
    assert!(store.job(&mid).await.unwrap().unwrap().log.is_none());
    assert!(store.job(&new).await.unwrap().unwrap().log.is_some());
    assert!(!log_file(d.path(), "logs/c1/mid.log"));
    assert!(log_file(d.path(), "logs/c1/new.log"));

    let everything = PruneLimits {
        now: Timestamp(at(0).0 + 1000 * DAY_MS),
        log_max_age_ms: 0,
        log_max_bytes: 0,
        row_max_age_ms: 365 * DAY_MS,
    };
    let report = store.prune(everything).await.unwrap();
    assert_eq!((report.logs_removed, report.rows_removed), (1, 4));
    let live_record = store.job(&live).await.unwrap().expect("active run kept");
    assert_eq!(live_record.job.state, JobState::Running);
    assert_eq!(store.read_log(&live).await.unwrap().len(), 500_000);
    assert!(log_file(d.path(), "logs/c1/live.log"));
    assert!(!log_file(d.path(), "logs/c1/new.log"));
}

#[tokio::test]
async fn pruning_a_row_removes_its_child_rows_by_cascade() {
    let d = dir();
    let store = SqliteBrokerStore::open(&d.path().join("broker.db")).unwrap();
    run(&store, "gone", 0, Some(1), 10).await;
    let keep = run(&store, "keep", 0, None, 10).await;
    let conn = Connection::open(store.path()).unwrap();
    assert_eq!(count(&conn, "step"), 4);
    let limits = PruneLimits {
        now: Timestamp(at(0).0 + 400 * DAY_MS),
        log_max_age_ms: 30 * DAY_MS,
        log_max_bytes: u64::MAX,
        row_max_age_ms: 365 * DAY_MS,
    };
    let report = store.prune(limits).await.unwrap();
    assert_eq!(report.rows_removed, 1);
    assert_eq!(count(&conn, "job"), 1);
    assert_eq!(count(&conn, "step"), 2);
    assert_eq!(count(&conn, "job_spec"), 1);
    assert_eq!(count(&conn, "job_log"), 1);
    assert!(store.job(&keep).await.unwrap().is_some());
}

#[tokio::test]
async fn prune_persists_across_reopen_and_repeats_as_a_noop() {
    let d = dir();
    let store = SqliteBrokerStore::open(&d.path().join("broker.db")).unwrap();
    let key = run(&store, "a", 0, Some(1), 50).await;
    let limits = PruneLimits {
        now: Timestamp(at(0).0 + 40 * DAY_MS),
        log_max_age_ms: 30 * DAY_MS,
        log_max_bytes: u64::MAX,
        row_max_age_ms: 365 * DAY_MS,
    };
    assert_eq!(store.prune(limits).await.unwrap().logs_removed, 1);
    let store = store.reopen().unwrap();
    assert!(store.job(&key).await.unwrap().unwrap().log.is_none());
    assert_eq!(store.prune(limits).await.unwrap().logs_removed, 0);
}
