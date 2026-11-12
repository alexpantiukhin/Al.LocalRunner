mod common;

use common::{broker, dir};
use lr_abstractions::store_contract::fixtures::{at, code, intake, join, machine, T0};
use lr_abstractions::{
    BrokerStore, JobKey, MachineKind, OutboxKind, PruneLimits, StoreError, Transition,
};
use lr_core::{JobId, JobState, Timestamp};
use lr_store::SqliteBrokerStore;
use rusqlite::Connection;

fn index(s: JobState) -> usize {
    match s {
        JobState::Queued => 0,
        JobState::Running => 1,
        JobState::Cancelling => 2,
        JobState::Passed => 3,
        JobState::Failed => 4,
        JobState::Timeout => 5,
        JobState::Cancelled => 6,
        JobState::Lost => 7,
    }
}

const ALL: [JobState; 8] = [
    JobState::Queued,
    JobState::Running,
    JobState::Cancelling,
    JobState::Passed,
    JobState::Failed,
    JobState::Timeout,
    JobState::Cancelled,
    JobState::Lost,
];

#[test]
fn the_state_list_in_the_test_is_complete() {
    for (i, s) in ALL.iter().enumerate() {
        assert_eq!(index(*s), i);
    }
}

#[tokio::test]
async fn every_final_state_is_guarded_by_the_trigger_and_pruned_and_no_other() {
    for state in ALL {
        let store = broker();
        let i = intake("c1", "j1", "check", T0);
        let key = i.job.key.clone();
        store.accept(i).await.unwrap();
        let conn = Connection::open(store.path()).unwrap();
        let name = format!("{state:?}").to_lowercase();
        conn.execute("UPDATE job SET state = ?1, finished_at = 1000", [&name])
            .unwrap();
        let guarded = conn.execute("UPDATE job SET exit_code = 5", []).is_err();
        assert_eq!(guarded, state.is_final(), "trigger for {name}");
        let limits = PruneLimits {
            now: Timestamp(1_000_000),
            log_max_age_ms: u64::MAX,
            log_max_bytes: u64::MAX,
            row_max_age_ms: 0,
        };
        store.prune(limits).await.unwrap();
        let kept = store.job(&key).await.unwrap().is_some();
        assert_eq!(kept, !state.is_final(), "prune for {name}");
    }
}

async fn running(store: &SqliteBrokerStore, client: &str, job: &str) -> JobKey {
    let i = intake(client, job, "check", T0);
    let key = i.job.key.clone();
    store.accept(i).await.unwrap();
    store.grant(&key, "r1").await.unwrap();
    store
        .transition(&key, Transition::to(JobState::Running, at(1)))
        .await
        .unwrap();
    key
}

async fn log_path(store: &SqliteBrokerStore, key: &JobKey) -> String {
    store.job(key).await.unwrap().unwrap().log.unwrap().path
}

#[tokio::test]
async fn ids_differing_only_in_case_get_separate_log_files() {
    let d = dir();
    let store = SqliteBrokerStore::open(&d.path().join("broker.db")).unwrap();
    let upper = running(&store, "Client", "Job").await;
    let lower = running(&store, "client", "job").await;
    store.append_log(&upper, b"UP", at(2), 100).await.unwrap();
    store.append_log(&lower, b"low", at(2), 100).await.unwrap();
    assert_eq!(store.read_log(&upper).await.unwrap(), b"UP");
    assert_eq!(store.read_log(&lower).await.unwrap(), b"low");
    let (a, b) = (
        log_path(&store, &upper).await,
        log_path(&store, &lower).await,
    );
    assert_ne!(a.to_lowercase(), b.to_lowercase());
}

#[tokio::test]
async fn windows_device_names_are_not_used_as_file_names() {
    let d = dir();
    let store = SqliteBrokerStore::open(&d.path().join("broker.db")).unwrap();
    let devices = ["con", "nul", "com1", "lpt9", "aux", "prn"];
    for (client, job) in [
        ("con", "nul"),
        ("CON", "com1"),
        ("lpt9", "aux"),
        ("prn", "_nul"),
    ] {
        let key = running(&store, client, job).await;
        store.append_log(&key, b"x", at(2), 100).await.unwrap();
        assert_eq!(store.read_log(&key).await.unwrap(), b"x");
        let rel = log_path(&store, &key).await;
        for part in rel.split('/') {
            let stem = part.strip_suffix(".log").unwrap_or(part);
            assert!(!devices.contains(&stem), "{rel}");
        }
        assert!(d.path().join(&rel).is_file(), "{rel}");
    }
}

#[tokio::test]
async fn requeue_refuses_a_damaged_stored_order_and_changes_nothing() {
    let store = broker();
    let i = intake("c1", "j1", "check", T0);
    let key = i.job.key.clone();
    store.accept(i).await.unwrap();
    store.grant(&key, "r1").await.unwrap();
    let conn = Connection::open(store.path()).unwrap();
    for damaged in ["garbage", "{}", "{\"spec_json\": 5}"] {
        conn.execute("UPDATE job_spec SET order_json = ?1", [damaged])
            .unwrap();
        let err = store.requeue(&key).await.unwrap_err();
        let StoreError::Io(msg) = err else {
            panic!("{err:?}")
        };
        assert!(msg.contains("is damaged"), "{msg}");
        assert!(store.orders().await.unwrap().is_empty());
        let runner = store.job(&key).await.unwrap().unwrap().job.runner;
        assert_eq!(runner.as_deref(), Some("r1"));
    }
}

#[tokio::test]
async fn expiry_and_revocation_close_entries_but_keep_the_counter_row() {
    let store = broker();
    let job = JobId("j1".into());
    store
        .issue_code(code("h", MachineKind::Hub, 1))
        .await
        .unwrap();
    store
        .redeem(join("h", machine("c1", MachineKind::Hub, "k"), at(1)))
        .await
        .unwrap();
    for t in [1, 2] {
        store
            .push_result("c1", &job, OutboxKind::Chunk, "x".into(), at(t), 100)
            .await
            .unwrap();
    }
    assert_eq!(store.expire_results(at(100)).await.unwrap(), 2);
    let conn = Connection::open(store.path()).unwrap();
    let rows = |c: &Connection| -> Vec<(i64, bool)> {
        let mut stmt = c
            .prepare("SELECT seq, acked_at IS NOT NULL FROM result_outbox ORDER BY seq")
            .unwrap();
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        rows.map(Result::unwrap).collect()
    };
    assert_eq!(rows(&conn), vec![(2, true)]);
    store
        .push_result("c1", &job, OutboxKind::Chunk, "x".into(), at(200), 100)
        .await
        .unwrap();
    store.revoke("c1", at(300)).await.unwrap();
    assert_eq!(rows(&conn), vec![(3, true)]);
}
