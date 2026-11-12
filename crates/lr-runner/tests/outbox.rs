mod common;

use common::*;
use lr_abstractions::fakes::{JobScript, StepScript};
use lr_abstractions::{OutboxKind, RunnerStore};
use lr_core::{JobId, JobState, Timestamp};
use lr_proto::{
    classify_seq, BrokerToRunner, DedupKey, ProtocolError, ProtocolErrorCode, RunnerToBroker,
    SeqCheck, Sequenced, CHUNK_TEXT_LIMIT,
};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

const SEC: Duration = Duration::from_secs(1);

struct Dedup {
    last: HashMap<DedupKey, u64>,
    accepted: Vec<RunnerToBroker>,
    repeats: usize,
    gaps: usize,
}

impl Dedup {
    fn new() -> Dedup {
        Dedup {
            last: HashMap::new(),
            accepted: Vec::new(),
            repeats: 0,
            gaps: 0,
        }
    }

    fn take(&mut self, msg: RunnerToBroker) {
        let Some(mut key) = DedupKey::of("runner-a", &msg) else {
            return;
        };
        key.seq = 0;
        let last = self.last.entry(key).or_insert(0);
        match classify_seq(*last, msg.sequence().unwrap().1) {
            SeqCheck::Next => {
                *last += 1;
                self.accepted.push(msg);
            }
            SeqCheck::Repeat => self.repeats += 1,
            SeqCheck::Gap { .. } => self.gaps += 1,
        }
    }
}

#[tokio::test]
async fn outbox_replay_after_drop_has_no_duplicates_with_dedup() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script(
        "job-1",
        JobScript::new(vec![
            StepScript::new(5 * SEC, 0),
            StepScript::new(5 * SEC, 0),
        ]),
    );
    let (_c, _h) = rig.start();
    let mut b = Broker::accept(&rig).await;
    b.auto_ack = false;
    let mut dedup = Dedup::new();
    rig.settle().await;
    b.assign(
        "job-1",
        1,
        worktree_spec(
            "/w/tree",
            vec![step("one", "a", 60), step("two", "b", 60)],
            60,
        ),
    )
    .await;
    dedup.take(b.next_job_msg().await.unwrap());
    rig.net.connections(BROKER)[0].client.drop_next(1);
    rig.advance(5 * SEC).await;
    rig.advance(5 * SEC).await;
    for m in b.until_result().await {
        dedup.take(m);
    }
    assert_eq!(dedup.gaps, 2, "seq 3 and 4 arrive after the lost seq 2");
    b.send(BrokerToRunner::Error(ProtocolError {
        code: ProtocolErrorCode::SeqGap,
        message: "job job-1: expected seq 2, got 3".into(),
        supported: None,
    }))
    .await;
    rig.advance(SEC).await;
    let b2 = Broker::accept(&rig).await;
    for m in b2.until_result().await {
        dedup.take(m);
    }
    let seqs: Vec<u64> = dedup
        .accepted
        .iter()
        .map(|m| m.sequence().unwrap().1)
        .collect();
    assert_eq!(seqs, vec![1, 2, 3, 4]);
    assert!(dedup.repeats >= 1, "the replay repeats what was received");
    let results = dedup
        .accepted
        .iter()
        .filter(|m| matches!(m, RunnerToBroker::JobResult(_)))
        .count();
    assert_eq!(results, 1);
    assert!(matches!(&dedup.accepted[0], RunnerToBroker::Started(_)));
    assert!(
        matches!(&dedup.accepted[3], RunnerToBroker::JobResult(r) if r.state == JobState::Passed)
    );
}

#[tokio::test]
async fn seq_counts_from_one_for_every_job() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let spec = worktree_spec("/w/tree", vec![step("b", "make", 60)], 60);
    b.assign("job-1", 1, spec.clone()).await;
    let first = b.until_result().await;
    b.assign("job-2", 1, spec).await;
    let second = b.until_result().await;
    let seqs = |msgs: &[RunnerToBroker]| -> Vec<u64> {
        msgs.iter().map(|m| m.sequence().unwrap().1).collect()
    };
    assert_eq!(seqs(&first), vec![1, 2, 3]);
    assert_eq!(seqs(&second), vec![1, 2, 3]);
    assert!(second
        .iter()
        .all(|m| m.sequence().unwrap().0 == &JobId("job-2".into())));
}

#[tokio::test]
async fn ack_removes_entries_up_to_seq() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    let (_c, _h) = rig.start();
    let mut b = Broker::accept(&rig).await;
    b.auto_ack = false;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 60)], 60),
    )
    .await;
    b.until_result().await;
    rig.settle().await;
    assert_eq!(rig.pending().await.len(), 3);
    b.send(BrokerToRunner::Ack(lr_proto::Ack {
        job: JobId("job-1".into()),
        seq: 2,
    }))
    .await;
    rig.settle().await;
    let left = rig.pending().await;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].kind, OutboxKind::JobResult);
}

#[tokio::test]
async fn resume_reports_last_broker_seq_per_job() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    let (_c, _h) = rig.start();
    let mut b = Broker::accept(&rig).await;
    b.auto_ack = false;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 60)], 60),
    )
    .await;
    b.until_result().await;
    rig.net.cut(BROKER);
    rig.advance(SEC).await;
    let b2 = Broker::accept(&rig).await;
    let resume = b2.until(|m| matches!(m, RunnerToBroker::Resume(_))).await;
    let RunnerToBroker::Resume(r) = resume else {
        unreachable!()
    };
    assert_eq!(r.last_seq(&JobId("job-1".into())), 1);
}

#[tokio::test]
async fn resume_keeps_a_job_whose_received_seq_is_unreadable() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    let (_c, _h) = rig.start();
    let mut b = Broker::accept(&rig).await;
    b.auto_ack = false;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 60)], 60),
    )
    .await;
    b.until_result().await;
    rig.fail_last_seq
        .store(true, std::sync::atomic::Ordering::SeqCst);
    rig.net.cut(BROKER);
    rig.advance(SEC).await;
    let b2 = Broker::accept(&rig).await;
    let resume = b2.until(|m| matches!(m, RunnerToBroker::Resume(_))).await;
    let RunnerToBroker::Resume(r) = resume else {
        unreachable!()
    };
    let point = r.points.iter().find(|p| p.job.0 == "job-1");
    assert_eq!(point.map(|p| p.last_seq), Some(0), "{r:?}");
}

#[tokio::test]
async fn output_is_cut_into_chunks_within_the_text_limit() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    let big = vec![b'x'; CHUNK_TEXT_LIMIT + 10];
    rig.script(
        "job-1",
        JobScript::new(vec![StepScript::new(SEC, 0).output(&big)]),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 60)], 60),
    )
    .await;
    rig.advance(SEC).await;
    let msgs = b.until_result().await;
    let sizes: Vec<usize> = msgs
        .iter()
        .filter_map(|m| match m {
            RunnerToBroker::Chunk(c) => Some(c.text.len()),
            _ => None,
        })
        .collect();
    assert_eq!(sizes, vec![CHUNK_TEXT_LIMIT, 10]);
    assert!(msgs.iter().all(|m| match m {
        RunnerToBroker::Chunk(c) => c.step == Some(0),
        _ => true,
    }));
}

#[tokio::test]
async fn non_utf8_output_is_sent_as_lossy_text() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script(
        "job-1",
        JobScript::new(vec![StepScript::new(SEC, 0).output(b"ok \xff\xfe end\n")]),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 60)], 60),
    )
    .await;
    rig.advance(SEC).await;
    let text = chunks_text(&b.until_result().await);
    assert_eq!(text, "ok \u{FFFD}\u{FFFD} end\n");
}

#[tokio::test]
async fn finished_jobs_older_than_a_day_are_forgotten_at_start() {
    let rig = Rig::new();
    let job = JobId("old".into());
    let old = Timestamp(START - 25 * 3_600_000);
    rig.store
        .push_outbox(&job, OutboxKind::JobResult, "{}".into(), old)
        .await
        .unwrap();
    rig.store.ack_outbox(&job, 1).await.unwrap();
    rig.store.accept_seq(&job, 1).await.unwrap();
    let (_c, _h) = rig.start();
    let _b = Broker::accept(&rig).await;
    assert_eq!(rig.store.last_seq(&job).await.unwrap(), 0);
}
