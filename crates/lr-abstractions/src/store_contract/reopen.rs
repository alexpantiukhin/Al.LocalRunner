//! Contract cases that close and open a store again: what is acknowledged, counted or final
//! stays so. `reopen` takes the store and returns a new handle over the same data (SQLite:
//! drop the connection, open the same file).

use super::broker::running;
use super::fixtures::at;
use crate::store::{
    ActiveOrder, Applied, BrokerStore, HubStore, Inbound, OutboxKind, RunnerStore, Transition,
};
use lr_core::{JobId, JobState};

/// Received seq of both directions and the broker-to-runner counter survive a restart.
pub async fn broker_seq_counters_survive_reopen<S: BrokerStore>(store: S, reopen: impl Fn(S) -> S) {
    let key = running(&store, "c1", "j1").await;
    store.accept_seq(&key, Inbound::Runner, 1).await.unwrap();
    store.accept_seq(&key, Inbound::Runner, 2).await.unwrap();
    store.accept_seq(&key, Inbound::Hub, 1).await.unwrap();
    assert_eq!(store.next_seq_to_runner(&key).await.unwrap(), 1);
    let store = reopen(store);
    assert_eq!(store.last_seq(&key, Inbound::Runner).await.unwrap(), 2);
    assert_eq!(store.last_seq(&key, Inbound::Hub).await.unwrap(), 1);
    assert!(!store.accept_seq(&key, Inbound::Runner, 2).await.unwrap());
    assert_eq!(store.next_seq_to_runner(&key).await.unwrap(), 2);
}

/// The result outbox survives a restart, an acknowledged entry stays gone and the seq goes on.
pub async fn broker_outbox_survives_reopen_and_ack_stays_acked<S: BrokerStore>(
    store: S,
    reopen: impl Fn(S) -> S,
) {
    let job = JobId("j1".into());
    for _ in 0..3 {
        store
            .push_result("c1", &job, OutboxKind::Chunk, "x".into(), at(1), 1 << 20)
            .await
            .unwrap();
    }
    store.ack_results("c1", &job, 2).await.unwrap();
    let store = reopen(store);
    let pending = store.pending_results("c1").await.unwrap();
    assert_eq!(pending.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3]);
    let next = store
        .push_result(
            "c1",
            &job,
            OutboxKind::JobResult,
            "r".into(),
            at(2),
            1 << 20,
        )
        .await
        .unwrap();
    assert_eq!(next, Some(4));
}

/// A final state written before a restart is still the only one after it.
pub async fn broker_final_state_survives_reopen<S: BrokerStore>(store: S, reopen: impl Fn(S) -> S) {
    let key = running(&store, "c1", "j1").await;
    let mut passed = Transition::to(JobState::Passed, at(20));
    passed.exit_code = Some(0);
    store.transition(&key, passed).await.unwrap();
    let store = reopen(store);
    let failed = Transition::to(JobState::Failed, at(30));
    assert_eq!(
        store.transition(&key, failed).await.unwrap(),
        Applied::Ignored
    );
    let record = store.job(&key).await.unwrap().unwrap();
    assert_eq!(record.job.state, JobState::Passed);
    assert_eq!(record.job.finished_at, Some(at(20)));
}

/// The hub outbox, its seq counter and the received seq survive a restart.
pub async fn hub_outbox_and_seq_survive_reopen<S: HubStore>(store: S, reopen: impl Fn(S) -> S) {
    let id = JobId("j1".into());
    let order = ActiveOrder::new(id.clone(), "agent", "alex", 1, Some(1));
    store
        .enqueue(order, OutboxKind::Order, "o".into(), at(1))
        .await
        .unwrap();
    store
        .push_outbox(&id, OutboxKind::Cancel, "c".into(), at(2))
        .await
        .unwrap();
    store.ack_outbox(&id, 1).await.unwrap();
    store.accept_seq(&id, 1).await.unwrap();
    let store = reopen(store);
    let pending = store.pending_outbox().await.unwrap();
    assert_eq!(pending.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![2]);
    let next = store
        .push_outbox(&id, OutboxKind::Cancel, "c".into(), at(3))
        .await
        .unwrap();
    assert_eq!(next, 3);
    assert!(!store.accept_seq(&id, 1).await.unwrap());
    assert!(store.accept_seq(&id, 2).await.unwrap());
}

/// The runner outbox, its seq counter and the received seq survive a restart.
pub async fn runner_outbox_and_seq_survive_reopen<S: RunnerStore>(
    store: S,
    reopen: impl Fn(S) -> S,
) {
    let id = JobId("j1".into());
    for _ in 0..2 {
        store
            .push_outbox(&id, OutboxKind::Chunk, "x".into(), at(1))
            .await
            .unwrap();
    }
    store.ack_outbox(&id, 1).await.unwrap();
    store.accept_seq(&id, 1).await.unwrap();
    let store = reopen(store);
    let pending = store.pending_outbox().await.unwrap();
    assert_eq!(pending.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![2]);
    let next = store
        .push_outbox(&id, OutboxKind::JobResult, "r".into(), at(2))
        .await
        .unwrap();
    assert_eq!(next, 3);
    assert_eq!(store.last_seq(&id).await.unwrap(), 1);
}
