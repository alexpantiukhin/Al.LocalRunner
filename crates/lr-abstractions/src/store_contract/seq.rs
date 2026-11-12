//! Contract cases of message sequence numbers (spec 3.9): one counter per run and direction,
//! the next seq accepted, repeats dropped, gaps refused.

use super::fixtures::{at, intake, T0};
use crate::store::{
    ActiveOrder, BrokerStore, HubStore, Inbound, JobKey, OutboxKind, RunnerStore, StoreError,
};
use lr_core::JobId;

async fn queued(store: &dyn BrokerStore, client: &str, job: &str) -> JobKey {
    let i = intake(client, job, "check", T0);
    let key = i.job.key.clone();
    store.accept(i).await.unwrap();
    key
}

/// Messages from the hub and from the runner of a run count separately, and per client key.
pub async fn broker_seq_counted_per_run_and_direction(store: &dyn BrokerStore) {
    let a = queued(store, "c1", "j1").await;
    let b = queued(store, "c2", "j1").await;
    assert_eq!(store.last_seq(&a, Inbound::Runner).await.unwrap(), 0);
    assert!(store.accept_seq(&a, Inbound::Runner, 1).await.unwrap());
    assert!(!store.accept_seq(&a, Inbound::Runner, 1).await.unwrap());
    assert!(store.accept_seq(&a, Inbound::Runner, 2).await.unwrap());
    assert!(!store.accept_seq(&a, Inbound::Runner, 1).await.unwrap());
    assert!(store.accept_seq(&a, Inbound::Hub, 1).await.unwrap());
    assert_eq!(store.last_seq(&a, Inbound::Runner).await.unwrap(), 2);
    assert_eq!(store.last_seq(&a, Inbound::Hub).await.unwrap(), 1);
    assert!(store.accept_seq(&b, Inbound::Runner, 1).await.unwrap());
    assert!(matches!(
        store
            .accept_seq(&JobKey::new("c1", "nope"), Inbound::Runner, 1)
            .await,
        Err(StoreError::NotFound(_))
    ));
}

/// A seq beyond the next is a protocol error and changes nothing.
pub async fn broker_seq_gap_is_refused(store: &dyn BrokerStore) {
    let a = queued(store, "c1", "j1").await;
    store.accept_seq(&a, Inbound::Runner, 1).await.unwrap();
    assert_eq!(
        store.accept_seq(&a, Inbound::Runner, 3).await,
        Err(StoreError::SeqGap {
            expected: 2,
            got: 3
        })
    );
    assert_eq!(store.last_seq(&a, Inbound::Runner).await.unwrap(), 1);
    assert!(store.accept_seq(&a, Inbound::Runner, 2).await.unwrap());
}

/// Messages from the broker to a runner get their own seq per run, from 1.
pub async fn broker_to_runner_seq_counts_per_run(store: &dyn BrokerStore) {
    let a = queued(store, "c1", "j1").await;
    let b = queued(store, "c1", "j2").await;
    assert_eq!(store.next_seq_to_runner(&a).await.unwrap(), 1);
    assert_eq!(store.next_seq_to_runner(&a).await.unwrap(), 2);
    assert_eq!(store.next_seq_to_runner(&b).await.unwrap(), 1);
}

/// The hub refuses a gap in what the broker sends.
pub async fn hub_seq_gap_is_refused(store: &dyn HubStore) {
    let id = JobId("j1".into());
    let order = ActiveOrder::new(id.clone(), "agent", "alex", 1, Some(1));
    store
        .enqueue(order, OutboxKind::Order, "o".into(), at(1))
        .await
        .unwrap();
    assert!(store.accept_seq(&id, 1).await.unwrap());
    assert_eq!(
        store.accept_seq(&id, 5).await,
        Err(StoreError::SeqGap {
            expected: 2,
            got: 5
        })
    );
    assert_eq!(store.active(&id).await.unwrap().unwrap().last_seq, 1);
}

/// The runner counts what the broker sends per job, drops repeats and refuses gaps.
pub async fn runner_received_seq_per_job(store: &dyn RunnerStore) {
    let (j1, j2) = (JobId("j1".into()), JobId("j2".into()));
    assert_eq!(store.last_seq(&j1).await.unwrap(), 0);
    assert!(store.accept_seq(&j1, 1).await.unwrap());
    assert!(!store.accept_seq(&j1, 1).await.unwrap());
    assert!(store.accept_seq(&j2, 1).await.unwrap());
    assert_eq!(
        store.accept_seq(&j1, 3).await,
        Err(StoreError::SeqGap {
            expected: 2,
            got: 3
        })
    );
    assert_eq!(store.last_seq(&j1).await.unwrap(), 1);
}

/// Outbox counters of different jobs are independent.
pub async fn runner_outbox_counters_independent_across_jobs(store: &dyn RunnerStore) {
    let (j1, j2) = (JobId("j1".into()), JobId("j2".into()));
    let push = |job: JobId| async move {
        store
            .push_outbox(&job, OutboxKind::Chunk, "x".into(), at(1))
            .await
            .unwrap()
    };
    assert_eq!(push(j1.clone()).await, 1);
    assert_eq!(push(j1.clone()).await, 2);
    assert_eq!(push(j2.clone()).await, 1);
    store.ack_outbox(&j2, 1).await.unwrap();
    assert_eq!(push(j1).await, 3);
    assert_eq!(push(j2).await, 2);
}
