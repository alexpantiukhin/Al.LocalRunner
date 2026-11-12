mod common;

use common::{clock, ipc_peer, job, settle, step, T0};
use lr_abstractions::fakes::{
    channel_pair, FakeProcessRunner, JobScript, MemoryHubStore, RecordingOutput, StepScript,
};
use lr_abstractions::{
    stop_signal, ActiveOrder, Frame, HubStore, OutboxKind, ProcessRunner, Transport,
};
use lr_core::{JobId, JobState, StepState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn manual_clock_wakes_sleepers_on_advance() {
    let (manual, clock) = clock();
    let woke = Arc::new(AtomicBool::new(false));
    let flag = woke.clone();
    let sleeper = clock.clone();
    let task = tokio::spawn(async move {
        sleeper.sleep(Duration::from_secs(10)).await;
        flag.store(true, Ordering::SeqCst);
    });
    manual.wait_for_sleepers(1).await;
    manual.advance(Duration::from_secs(9));
    settle().await;
    assert!(!woke.load(Ordering::SeqCst), "woke before its deadline");
    manual.advance(Duration::from_secs(1));
    task.await.unwrap();
    assert!(woke.load(Ordering::SeqCst));
    assert_eq!(clock.now(), T0.plus_secs(10));
}

#[tokio::test]
async fn memory_store_outbox_ack_removes_entry() {
    let store = MemoryHubStore::new();
    let id = JobId("j1".into());
    let order = ActiveOrder::new(id.clone(), "agent", "alex", 42, Some(7));
    let seq = store
        .enqueue(order, OutboxKind::Order, "{}".into(), T0)
        .await
        .unwrap();
    assert_eq!(store.pending_outbox().await.unwrap().len(), 1);
    assert_eq!(store.ack_outbox(&id, seq).await.unwrap(), 1);
    assert!(store.pending_outbox().await.unwrap().is_empty());
    assert_eq!(store.ack_outbox(&id, seq).await.unwrap(), 0);
}

#[tokio::test]
async fn channel_pair_can_duplicate_and_drop_frames() {
    let (_, clock) = clock();
    let (a, b) = channel_pair(clock, ipc_peer("a"), ipc_peer("b"));
    a.faults().drop_next(1);
    a.send(Frame::from_bytes(b"1".to_vec())).await.unwrap();
    a.faults().duplicate_next(1);
    a.send(Frame::from_bytes(b"2".to_vec())).await.unwrap();
    a.send(Frame::from_bytes(b"3".to_vec())).await.unwrap();
    a.send(Frame::from_bytes(b"end".to_vec())).await.unwrap();
    let mut got = Vec::new();
    for _ in 0..3 {
        got.push(b.recv().await.unwrap().into_bytes());
    }
    assert_eq!(got, vec![b"2".to_vec(), b"2".to_vec(), b"3".to_vec()]);
}

#[tokio::test]
async fn fake_process_runner_finishes_after_scripted_time() {
    let (manual, clock) = clock();
    let runner = Arc::new(FakeProcessRunner::new(clock.clone()));
    let spec = job("j1", 60, vec![step("build", 30)]);
    runner.script(
        spec.job.clone(),
        JobScript::new(vec![StepScript::new(Duration::from_secs(5), 0)]),
    );
    let output = Arc::new(RecordingOutput::new());
    let (_handle, stop) = stop_signal();
    let done = Arc::new(AtomicBool::new(false));
    let (r, o, d) = (runner.clone(), output.clone(), done.clone());
    let task = tokio::spawn(async move {
        let outcome = r.run_job(spec, o.as_ref(), stop).await;
        d.store(true, Ordering::SeqCst);
        outcome
    });
    manual.wait_for_sleepers(1).await;
    manual.advance(Duration::from_secs(4));
    settle().await;
    assert!(
        !done.load(Ordering::SeqCst),
        "finished before scripted time"
    );
    manual.advance(Duration::from_secs(1));
    let outcome = task.await.unwrap();
    assert_eq!(outcome.state, JobState::Passed);
    assert_eq!(outcome.steps[0].state, StepState::Passed);
    assert_eq!(outcome.steps[0].started_at, Some(T0));
    assert_eq!(outcome.steps[0].finished_at, Some(T0.plus_secs(5)));
}
