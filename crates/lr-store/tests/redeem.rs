mod common;

use common::broker;
use lr_abstractions::store_contract::fixtures::{at, code, join, machine};
use lr_abstractions::{BrokerStore, MachineKind, StoreError};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_single_use_code_admits_exactly_one_of_many_concurrent_joins() {
    let store = Arc::new(broker());
    store
        .issue_code(code("h", MachineKind::Hub, 1))
        .await
        .unwrap();
    let tasks: Vec<_> = (0..12)
        .map(|n| {
            let store = store.clone();
            tokio::spawn(async move {
                let m = machine(&format!("m{n}"), MachineKind::Hub, &format!("k{n}"));
                store.redeem(join("h", m, at(1))).await
            })
        })
        .collect();
    let (mut joined, mut used_up) = (0, 0);
    for t in tasks {
        match t.await.unwrap() {
            Ok(_) => joined += 1,
            Err(StoreError::CodeUsedUp) => used_up += 1,
            Err(other) => panic!("{other:?}"),
        }
    }
    assert_eq!((joined, used_up), (1, 11));
    assert_eq!(store.machines().await.unwrap().len(), 1);
    assert_eq!(store.join_code("h").await.unwrap().unwrap().uses_left, 0);
}
