mod common;

use common::harness::*;
use lr_abstractions::{BrokerStore, JobFilter, JobKey, OutboxKind};
use lr_core::{JobId, JobState, Timestamp};
use lr_proto::*;
use std::time::Duration;

const ALEX: &str = "local:alex";

async fn running(h: &Harness) -> (Conn, Conn) {
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    hub.started_notice().await;
    (r, hub)
}

#[tokio::test]
async fn duplicate_order_same_key_jobid_seq_is_noop() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    let before = h.job_row(ALEX, "j1").await.unwrap();
    hub.send(&order("j1", req(512, 1, 60))).await;
    let again = hub.accepted().await;
    assert_eq!(again.job, JobId("j1".into()));
    assert_eq!(h.job_row(ALEX, "j1").await.unwrap(), before);
    assert_eq!(h.store.jobs(&JobFilter::default()).await.unwrap().len(), 1);
    r.quiet().await;
}

#[tokio::test]
async fn same_job_id_from_two_clients_is_two_runs() {
    let h = Harness::start("");
    let mut r1 = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut r2 = h
        .member_runner("r2", "10.0.0.8", caps(&["linux"], 8000, 8))
        .await;
    let mut alex = h.local_hub("alex").await;
    let mut bob = h.local_hub("bob").await;
    alex.send(&order("j1", req(512, 1, 60))).await;
    alex.accepted().await;
    bob.send(&order("j1", req(512, 1, 60))).await;
    bob.accepted().await;
    assert_eq!(r1.assign().await.spec.client, ALEX);
    assert_eq!(r2.assign().await.spec.client, "local:bob");
    assert!(h.job_row("local:bob", "j1").await.is_some());
    r1.send(&result("j1", 1, JobState::Passed, 0)).await;
    r1.acked(1).await;
    alex.sync().await;
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Passed
    );
    assert_eq!(
        h.job_row("local:bob", "j1").await.unwrap().job.state,
        JobState::Queued
    );
}

#[tokio::test]
async fn result_twice_one_history_row() {
    let h = Harness::start("");
    let (mut r, mut hub) = running(&h).await;
    h.clock.advance(Duration::from_secs(2));
    r.send(&result("j1", 2, JobState::Passed, 0)).await;
    r.acked(2).await;
    r.send(&result("j1", 2, JobState::Passed, 0)).await;
    h.clock.advance(Duration::from_secs(2));
    r.send(&result("j1", 3, JobState::Failed, 1)).await;
    let mut acks = 0;
    while acks < 2 {
        if let BrokerToRunner::Ack(_) = r.runner().await {
            acks += 1;
        }
    }
    let got = hub.sync().await;
    let results: Vec<_> = got
        .iter()
        .filter(|m| matches!(m, BrokerToHub::JobResult(_)))
        .collect();
    assert_eq!(results.len(), 1);
    let rows = h.store.jobs(&JobFilter::default()).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].job.state, JobState::Passed);
    assert_eq!(rows[0].job.exit_code, Some(0));
    assert_eq!(rows[0].job.finished_at, Some(Timestamp(START + 2000)));
}

#[tokio::test]
async fn repeated_started_keeps_the_first_start_time() {
    let h = Harness::start("");
    let (mut r, mut hub) = running(&h).await;
    h.clock.advance(Duration::from_secs(9));
    r.send(&started("j1", 1)).await;
    r.acked(1).await;
    r.acked(1).await;
    let got = hub.sync().await;
    assert!(got.iter().all(|m| !matches!(m, BrokerToHub::Started(_))));
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.started_at,
        Some(Timestamp(START))
    );
}

#[tokio::test]
async fn repeated_chunk_is_logged_once() {
    let h = Harness::start("");
    let (mut r, mut hub) = running(&h).await;
    r.send(&chunk("j1", 2, "once\n")).await;
    r.send(&chunk("j1", 2, "once\n")).await;
    r.send(&chunk("j1", 3, "twice\n")).await;
    r.acked(3).await;
    hub.sync().await;
    let log = h.store.read_log(&JobKey::new(ALEX, "j1")).await.unwrap();
    assert_eq!(log, b"once\ntwice\n");
}

#[tokio::test]
async fn runner_messages_are_acked_by_seq() {
    let h = Harness::start("");
    let (mut r, _hub) = running(&h).await;
    match r.runner().await {
        BrokerToRunner::Ack(a) => assert_eq!((a.job, a.seq), (JobId("j1".into()), 1)),
        other => panic!("expected ack, got {other:?}"),
    }
    r.send(&chunk("j1", 2, "x")).await;
    match r.runner().await {
        BrokerToRunner::Ack(a) => assert_eq!(a.seq, 2),
        other => panic!("expected ack, got {other:?}"),
    }
}

#[tokio::test]
async fn seq_gap_from_the_runner_is_a_protocol_error() {
    let h = Harness::start("");
    let (mut r, _hub) = running(&h).await;
    r.runner().await;
    r.send(&chunk("j1", 5, "x")).await;
    match r.runner().await {
        BrokerToRunner::Error(e) => assert_eq!(e.code, ProtocolErrorCode::SeqGap),
        other => panic!("expected error, got {other:?}"),
    }
    assert!(h
        .store
        .read_log(&JobKey::new(ALEX, "j1"))
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn hub_resume_resends_only_what_it_has_not_seen() {
    let h = Harness::start("");
    let (mut r, hub) = running(&h).await;
    hub.close().await;
    r.send(&chunk("j1", 2, "a")).await;
    r.send(&result("j1", 3, JobState::Passed, 0)).await;
    r.acked(3).await;
    let mut back = h.local_hub("alex").await;
    back.sync().await;
    back.send(&HubToBroker::Resume(Resume {
        points: vec![ResumePoint {
            job: JobId("j1".into()),
            last_seq: 1,
        }],
    }))
    .await;
    let got = back.sync().await;
    let seqs: Vec<u64> = got
        .iter()
        .filter_map(|m| match m {
            BrokerToHub::Chunk(c) => Some(c.seq),
            BrokerToHub::JobResult(r) => Some(r.seq),
            BrokerToHub::Started(s) => Some(s.seq),
            _ => None,
        })
        .collect();
    assert_eq!(seqs, vec![2, 3]);
    let pending = h.store.pending_results(ALEX).await.unwrap();
    assert_eq!(
        pending.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![2, 3]
    );
}

#[tokio::test]
async fn hub_ack_removes_outbox_entries() {
    let h = Harness::start("");
    let (_r, mut hub) = running(&h).await;
    hub.sync().await;
    assert_eq!(h.store.pending_results(ALEX).await.unwrap().len(), 1);
    hub.send(&HubToBroker::Ack(Ack {
        job: JobId("j1".into()),
        seq: 1,
    }))
    .await;
    hub.sync().await;
    assert!(h.store.pending_results(ALEX).await.unwrap().is_empty());
}

#[tokio::test]
async fn repeated_cancel_changes_nothing() {
    let h = Harness::start("");
    let (_r, mut hub) = running(&h).await;
    hub.send(&cancel("j1", 2)).await;
    hub.send(&cancel("j1", 2)).await;
    let got = hub.sync().await;
    let acks = got
        .iter()
        .filter(|m| matches!(m, BrokerToHub::Ack(a) if a.seq == 2))
        .count();
    assert_eq!(acks, 2);
    let cancels = h
        .store
        .pending_results(ALEX)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == OutboxKind::CancelAck)
        .count();
    assert_eq!(cancels, 1);
}

#[tokio::test]
async fn outbox_drops_chunks_over_the_limit_but_keeps_the_result() {
    let h = Harness::start("[limits]\noutbox_max_mb = 1\n");
    let (mut r, hub) = running(&h).await;
    hub.close().await;
    let big = "x".repeat(600 * 1024);
    r.send(&chunk("j1", 2, &big)).await;
    r.send(&chunk("j1", 3, &big)).await;
    r.send(&result("j1", 4, JobState::Passed, 0)).await;
    r.acked(4).await;
    let mut back = h.local_hub("alex").await;
    back.sync().await;
    let kinds: Vec<OutboxKind> = h
        .store
        .pending_results(ALEX)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![
            OutboxKind::Started,
            OutboxKind::Chunk,
            OutboxKind::JobResult
        ]
    );
    let log = h.store.read_log(&JobKey::new(ALEX, "j1")).await.unwrap();
    assert_eq!(log.len(), 2 * big.len());
}

#[tokio::test]
async fn job_log_is_cut_at_its_limit() {
    let h = Harness::start("[limits]\njob_log_max_mb = 1\n");
    let (mut r, mut hub) = running(&h).await;
    let big = "y".repeat(600 * 1024);
    r.send(&chunk("j1", 2, &big)).await;
    r.send(&chunk("j1", 3, &big)).await;
    r.acked(3).await;
    hub.sync().await;
    let log = h.store.read_log(&JobKey::new(ALEX, "j1")).await.unwrap();
    assert!(log.len() <= 1024 * 1024 + lr_abstractions::LOG_TRUNCATED_MARK.len());
    assert!(log.ends_with(lr_abstractions::LOG_TRUNCATED_MARK));
}
