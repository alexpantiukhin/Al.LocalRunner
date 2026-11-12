mod common;

use common::runner;
use lr_abstractions::{OutboxKind, RunnerStore};
use lr_core::{JobId, Timestamp};

#[tokio::test]
async fn finished_jobs_leave_no_rows_in_runner_db_after_the_keep_period() {
    let store = runner();
    let (done, live) = (JobId("done".into()), JobId("live".into()));
    for job in [&done, &live] {
        store.accept_seq(job, 1).await.unwrap();
        store
            .push_outbox(job, OutboxKind::Chunk, "x".into(), Timestamp(1))
            .await
            .unwrap();
    }
    store
        .push_outbox(&done, OutboxKind::JobResult, "r".into(), Timestamp(2))
        .await
        .unwrap();
    store.ack_outbox(&done, 2).await.unwrap();
    assert_eq!(store.forget_finished(Timestamp(10)).await.unwrap(), 1);
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    let outbox = |job: &str| {
        count(&format!(
            "SELECT count(*) FROM runner_outbox WHERE job_id = '{job}'"
        ))
    };
    let recv = |job: &str| {
        count(&format!(
            "SELECT count(*) FROM runner_recv WHERE job_id = '{job}'"
        ))
    };
    assert_eq!((outbox("done"), recv("done")), (0, 0));
    assert_eq!((outbox("live"), recv("live")), (1, 1));
}
