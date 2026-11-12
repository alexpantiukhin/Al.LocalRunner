mod common;

use common::{broker, dir, hub};
use lr_abstractions::store_contract::fixtures::{at, intake, T0};
use lr_abstractions::{ActiveOrder, Admission, BrokerStore, HubStore, JobKey, OutboxKind};
use lr_core::JobId;
use lr_store::SqliteBrokerStore;
use rusqlite::Connection;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_pushes_get_distinct_consecutive_seqs() {
    let store = Arc::new(broker());
    let job = JobId("j1".into());
    let tasks: Vec<_> = (0..64)
        .map(|n| {
            let (store, job) = (store.clone(), job.clone());
            tokio::spawn(async move {
                store
                    .push_result(
                        "c1",
                        &job,
                        OutboxKind::Chunk,
                        format!("{n}"),
                        at(1),
                        1 << 20,
                    )
                    .await
                    .unwrap()
                    .unwrap()
            })
        })
        .collect();
    let mut seqs = Vec::new();
    for t in tasks {
        seqs.push(t.await.unwrap());
    }
    seqs.sort_unstable();
    assert_eq!(seqs, (1..=64).collect::<Vec<u64>>());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_duplicate_orders_are_stored_once() {
    let store = Arc::new(broker());
    let tasks: Vec<_> = (0..16)
        .map(|_| {
            let store = store.clone();
            tokio::spawn(
                async move { store.accept(intake("c1", "j1", "check", T0)).await.unwrap() },
            )
        })
        .collect();
    let mut accepted = 0;
    for t in tasks {
        if t.await.unwrap() == Admission::Accepted {
            accepted += 1;
        }
    }
    assert_eq!(accepted, 1);
    assert_eq!(store.orders().await.unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_seq_acceptance_lets_exactly_one_caller_take_each_seq() {
    let store = Arc::new(broker());
    store.accept(intake("c1", "j1", "check", T0)).await.unwrap();
    let key = JobKey::new("c1", "j1");
    let tasks: Vec<_> = (0..24)
        .map(|_| {
            let (store, key) = (store.clone(), key.clone());
            tokio::spawn(async move {
                store
                    .accept_seq(&key, lr_abstractions::Inbound::Runner, 1)
                    .await
                    .unwrap()
            })
        })
        .collect();
    let mut taken = 0;
    for t in tasks {
        taken += usize::from(t.await.unwrap());
    }
    assert_eq!(taken, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_calls_from_many_tasks_do_not_lose_updates() {
    let store = Arc::new(hub());
    let tasks: Vec<_> = (0..20)
        .map(|n| {
            let store = store.clone();
            tokio::spawn(async move {
                let order = ActiveOrder::new(JobId(format!("j{n}")), "a", "u", 1, None);
                store
                    .enqueue(order, OutboxKind::Order, "o".into(), at(1))
                    .await
                    .unwrap()
            })
        })
        .collect();
    for t in tasks {
        assert_eq!(t.await.unwrap(), 1);
    }
    assert_eq!(store.active_orders().await.unwrap().len(), 20);
    assert_eq!(store.pending_outbox().await.unwrap().len(), 20);
}

#[tokio::test(flavor = "current_thread")]
async fn a_store_call_waiting_on_the_database_does_not_block_the_runtime() {
    let d = dir();
    let path = d.path().join("broker.db");
    let store = Arc::new(SqliteBrokerStore::open(&path).unwrap());
    let other = Connection::open(&path).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    let call = {
        let store = store.clone();
        tokio::spawn(async move { store.accept(intake("c1", "j1", "check", T0)).await })
    };
    let mut ticks = 0;
    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        ticks += 1;
        assert!(!call.is_finished(), "the call must wait for the write lock");
    }
    assert_eq!(ticks, 6);
    other.execute_batch("COMMIT").unwrap();
    assert_eq!(call.await.unwrap().unwrap(), Admission::Accepted);
}

#[tokio::test]
async fn two_handles_over_one_file_see_each_others_writes() {
    let d = dir();
    let path = d.path().join("broker.db");
    let store = SqliteBrokerStore::open(&path).unwrap();
    let again = SqliteBrokerStore::open_existing(&path).unwrap();
    store.accept(intake("c1", "j1", "check", T0)).await.unwrap();
    assert!(again.job(&JobKey::new("c1", "j1")).await.unwrap().is_some());
}
