mod common;

use common::{broker, dir};
use lr_abstractions::store_contract::fixtures::{at, code, intake, join, machine, T0};
use lr_abstractions::{
    Admission, Applied, BrokerStore, GithubRunRow, JobKey, LogAppend, MachineKind, StoreError,
    Transition, LOG_TRUNCATED_MARK,
};
use lr_core::JobState;
use lr_store::SqliteBrokerStore;
use rusqlite::Connection;

fn side(store: &SqliteBrokerStore) -> Connection {
    Connection::open(store.path()).unwrap()
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

#[tokio::test]
async fn final_state_is_guarded_by_the_database_itself() {
    let store = broker();
    let key = running(&store, "c1", "j1").await;
    let mut done = Transition::to(JobState::Passed, at(5));
    done.exit_code = Some(0);
    store.transition(&key, done).await.unwrap();
    let conn = side(&store);
    for sql in [
        "UPDATE job SET state = 'failed'",
        "UPDATE job SET finished_at = 1",
        "UPDATE job SET exit_code = 9",
    ] {
        let err = conn.execute(sql, []).unwrap_err().to_string();
        assert!(err.contains("written once"), "{sql}: {err}");
    }
    let record = store.job(&key).await.unwrap().unwrap();
    assert_eq!(record.job.state, JobState::Passed);
    assert_eq!(record.job.exit_code, Some(0));
}

#[tokio::test]
async fn final_state_and_its_details_stay_after_reopen_and_late_reports() {
    let store = broker();
    let key = running(&store, "c1", "j1").await;
    let mut cancelling = Transition::to(JobState::Cancelling, at(4));
    cancelling.cancelled_by = Some("alex".into());
    cancelling.cancel_reason = Some("stale".into());
    store.transition(&key, cancelling).await.unwrap();
    let mut cancel = Transition::to(JobState::Cancelled, at(5));
    cancel.exit_code = Some(143);
    store.transition(&key, cancel).await.unwrap();
    let store = store.reopen().unwrap();
    let mut late = Transition::to(JobState::Passed, at(9));
    late.exit_code = Some(0);
    late.cancelled_by = Some("bob".into());
    assert_eq!(
        store.transition(&key, late).await.unwrap(),
        Applied::Ignored
    );
    let job = store.job(&key).await.unwrap().unwrap().job;
    assert_eq!(job.state, JobState::Cancelled);
    assert_eq!(job.exit_code, Some(143));
    assert_eq!(job.finished_at, Some(at(5)));
    assert_eq!(job.cancelled_by.as_deref(), Some("alex"));
    assert_eq!(job.cancel_reason.as_deref(), Some("stale"));
}

#[tokio::test]
async fn order_row_is_removed_by_grant_in_the_same_transaction_that_records_the_runner() {
    let store = broker();
    let i = intake("c1", "j1", "check", T0);
    let key = i.job.key.clone();
    store.accept(i).await.unwrap();
    let conn = side(&store);
    let orders = |c: &Connection| -> i64 {
        c.query_row("SELECT count(*) FROM \"order\"", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(orders(&conn), 1);
    conn.execute_batch(
        "CREATE TRIGGER boom BEFORE UPDATE OF runner_name ON job \
         BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
    )
    .unwrap();
    assert!(matches!(
        store.grant(&key, "r1").await,
        Err(StoreError::Io(_))
    ));
    assert_eq!(orders(&conn), 1, "a failed grant must not lose the order");
    assert_eq!(store.job(&key).await.unwrap().unwrap().job.runner, None);
    conn.execute_batch("DROP TRIGGER boom").unwrap();
    store.grant(&key, "r1").await.unwrap();
    assert_eq!(orders(&conn), 0);
    let runner: String = conn
        .query_row("SELECT runner_name FROM job", [], |r| r.get(0))
        .unwrap();
    assert_eq!(runner, "r1");
}

#[tokio::test]
async fn accept_stores_everything_or_nothing() {
    let store = broker();
    let conn = side(&store);
    conn.execute_batch(
        "CREATE TRIGGER boom BEFORE INSERT ON \"order\" \
         BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
    )
    .unwrap();
    let i = intake("c1", "j1", "check", T0);
    let key = i.job.key.clone();
    assert!(store.accept(i.clone()).await.is_err());
    assert!(store.job(&key).await.unwrap().is_none());
    let steps: i64 = conn
        .query_row("SELECT count(*) FROM step", [], |r| r.get(0))
        .unwrap();
    assert_eq!(steps, 0);
    conn.execute_batch("DROP TRIGGER boom").unwrap();
    assert_eq!(store.accept(i).await.unwrap(), Admission::Accepted);
}

#[tokio::test]
async fn redeem_spends_a_use_only_together_with_the_key_record() {
    let store = broker();
    store
        .issue_code(code("h", MachineKind::Hub, 1))
        .await
        .unwrap();
    let conn = side(&store);
    conn.execute_batch(
        "CREATE TRIGGER boom BEFORE UPDATE ON join_code \
         BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
    )
    .unwrap();
    let m = machine("laptop", MachineKind::Hub, "k1");
    assert!(store.redeem(join("h", m.clone(), at(1))).await.is_err());
    assert!(store.machines().await.unwrap().is_empty());
    assert_eq!(store.join_code("h").await.unwrap().unwrap().uses_left, 1);
    conn.execute_batch("DROP TRIGGER boom").unwrap();
    store.redeem(join("h", m, at(2))).await.unwrap();
}

#[tokio::test]
async fn github_facts_note_and_requeue_snapshot_survive_reopen() {
    let store = broker();
    let mut i = intake("c1", "j1", "check", T0);
    i.job.note = Some("fix flaky test".into());
    i.job.source = lr_abstractions::SourceKind::Github;
    i.github = Some(GithubRunRow {
        repository: "owner/repo".into(),
        workflow: "ci".into(),
        job_name: "build".into(),
        run_id: u64::MAX - 3,
        attempt: Some(2),
        git_ref: "refs/heads/main".into(),
        actor: "alex".into(),
        run_url: "https://example.test/run/1".into(),
    });
    i.order.same_machine = true;
    i.order.priority_hint = Some(-4);
    let (key, github, order) = (i.job.key.clone(), i.github.clone(), i.order.clone());
    store.accept(i).await.unwrap();
    let store = store.reopen().unwrap();
    let record = store.job(&key).await.unwrap().unwrap();
    assert_eq!(record.github, github);
    assert_eq!(record.job.note.as_deref(), Some("fix flaky test"));
    store.grant(&key, "r1").await.unwrap();
    store.requeue(&key).await.unwrap();
    assert_eq!(store.orders().await.unwrap(), vec![order]);
}

#[tokio::test]
async fn logs_live_in_files_next_to_the_database_and_survive_reopen() {
    let d = dir();
    let db = d.path().join("broker.db");
    let store = SqliteBrokerStore::open(&db).unwrap();
    let key = running(&store, "c1", "j1").await;
    store.append_log(&key, b"one\n", at(2), 1024).await.unwrap();
    let rel = store.job(&key).await.unwrap().unwrap().log.unwrap().path;
    assert!(rel.starts_with("logs/"), "{rel}");
    assert_eq!(std::fs::read(d.path().join(&rel)).unwrap(), b"one\n");
    let store = store.reopen().unwrap();
    store.append_log(&key, b"two\n", at(3), 1024).await.unwrap();
    assert_eq!(store.read_log(&key).await.unwrap(), b"one\ntwo\n");
}

#[tokio::test]
async fn a_cut_log_stays_cut_after_reopen() {
    let store = broker();
    let key = running(&store, "c1", "j1").await;
    store.append_log(&key, b"12345", at(2), 8).await.unwrap();
    assert_eq!(
        store.append_log(&key, b"67890", at(3), 8).await.unwrap(),
        LogAppend::Truncated
    );
    let store = store.reopen().unwrap();
    assert_eq!(
        store.append_log(&key, b"more", at(4), 8).await.unwrap(),
        LogAppend::Dropped
    );
    let mut want = b"12345678".to_vec();
    want.extend_from_slice(LOG_TRUNCATED_MARK);
    assert_eq!(store.read_log(&key).await.unwrap(), want);
}

#[tokio::test]
async fn bytes_left_in_the_file_by_an_interrupted_append_are_ignored_and_cut() {
    let d = dir();
    let store = SqliteBrokerStore::open(&d.path().join("broker.db")).unwrap();
    let key = running(&store, "c1", "j1").await;
    store
        .append_log(&key, b"good\n", at(2), 1024)
        .await
        .unwrap();
    let rel = store.job(&key).await.unwrap().unwrap().log.unwrap().path;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(d.path().join(&rel))
        .unwrap();
    std::io::Write::write_all(&mut file, b"stray").unwrap();
    drop(file);
    assert_eq!(store.read_log(&key).await.unwrap(), b"good\n");
    store
        .append_log(&key, b"next\n", at(3), 1024)
        .await
        .unwrap();
    assert_eq!(store.read_log(&key).await.unwrap(), b"good\nnext\n");
    assert_eq!(std::fs::read(d.path().join(&rel)).unwrap(), b"good\nnext\n");
}

#[tokio::test]
async fn hostile_ids_cannot_move_a_log_out_of_the_logs_directory() {
    let d = dir();
    let store = SqliteBrokerStore::open(&d.path().join("state").join("broker.db")).unwrap();
    let key = running(&store, "../../c", "../../../evil").await;
    store.append_log(&key, b"x", at(2), 1024).await.unwrap();
    let rel = store.job(&key).await.unwrap().unwrap().log.unwrap().path;
    assert!(!rel.contains(".."), "{rel}");
    assert!(d.path().join("state").join(&rel).is_file());
    let outside: Vec<_> = std::fs::read_dir(d.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(outside, vec!["state".to_string()]);
}

#[tokio::test]
async fn log_of_unknown_run_is_not_found() {
    let store = broker();
    let key = JobKey::new("c1", "nope");
    assert!(matches!(
        store.append_log(&key, b"x", at(1), 10).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store.read_log(&key).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn registry_survives_reopen_with_revocation_and_runners() {
    let store = broker();
    store
        .issue_code(code("h", MachineKind::Runner, 2))
        .await
        .unwrap();
    store
        .redeem(join("h", machine("box", MachineKind::Runner, "k"), at(1)))
        .await
        .unwrap();
    let mut r = lr_abstractions::store_contract::fixtures::runner("light", "box");
    r.tags.insert("fast".into());
    r.accept_min_mem_mb = Some(512);
    r.services.insert("emulator".into(), true);
    store.upsert_runner(r.clone()).await.unwrap();
    store.revoke("box", at(9)).await.unwrap();
    let store = store.reopen().unwrap();
    assert_eq!(store.machines().await.unwrap()[0].revoked_at, Some(at(9)));
    r.state = lr_abstractions::RunnerState::Removed;
    assert_eq!(store.runners().await.unwrap(), vec![r]);
    assert_eq!(store.join_code("h").await.unwrap().unwrap().uses_left, 1);
}
