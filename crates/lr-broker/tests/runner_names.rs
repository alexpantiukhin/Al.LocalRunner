mod common;

use common::harness::*;
use lr_abstractions::{BrokerStore, MachineKind, RunnerRow, RunnerState};
use lr_core::{JobId, Timestamp};
use lr_proto::*;
use std::collections::{BTreeMap, BTreeSet};

async fn stored_runner(h: &Harness, name: &str) {
    h.seed(name, MachineKind::Runner, "").await;
    h.store
        .upsert_runner(RunnerRow {
            name: name.into(),
            machine_name: name.into(),
            programs: BTreeSet::new(),
            platforms: BTreeSet::from(["linux".to_string()]),
            tags: BTreeSet::new(),
            max_timeout_s: None,
            cache_max_mb: 0,
            cache_used_mb: 0,
            free_disk_mb: 0,
            services: BTreeMap::new(),
            accept_min_mem_mb: None,
            last_seen: Some(Timestamp(START)),
            state: RunnerState::Offline,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn local_runner_cannot_take_the_name_of_a_stored_network_runner() {
    let h = Harness::start("");
    stored_runner(&h, "r1").await;
    let _impostor = h.local_runner("alex", "r1", linux()).await;
    let mut real = h.dial(member_peer("pk-r1", "10.0.0.7")).await;
    real.send(&hello("r1", Some(linux()))).await;
    real.welcome().await;
    let mut hub = h.local_hub("alex").await;
    let ReadReply::Runners { runners } = hub.read(ReadRequest::Runners).await else {
        panic!("runners");
    };
    let names: Vec<_> = runners.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&"r1") && names.contains(&"local:r1"),
        "{names:?}"
    );
    let row = h.store.runners().await.unwrap();
    assert_eq!(row.len(), 1);
    assert_eq!(row[0].machine_name, "r1");
}

#[tokio::test]
async fn network_runner_keeps_its_name_while_a_local_one_uses_the_same() {
    let h = Harness::start("");
    let _local = h.local_runner("alex", "r1", linux()).await;
    let _real = h.member_runner("r1", "10.0.0.7", linux()).await;
    assert_eq!(
        h.store.runners().await.unwrap()[0].state,
        RunnerState::Online
    );
}

#[tokio::test]
async fn history_shows_a_local_runner_under_its_local_name() {
    let h = Harness::start("");
    let mut local = h.local_runner("alex", "r1", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", same_machine_req())).await;
    hub.accepted().await;
    assert_eq!(local.assign().await.job, JobId("j1".into()));
    let row = h.job_row("local:alex", "j1").await.unwrap().job;
    assert_eq!(row.runner.as_deref(), Some("local:r1"));
}

#[tokio::test]
async fn network_runner_name_with_a_colon_is_refused() {
    let h = Harness::start("");
    let key = h.seed("m1", MachineKind::Runner, "").await;
    let mut c = h.dial(member_peer(&key, "10.0.0.7")).await;
    c.send(&hello("local:r1", Some(linux()))).await;
    match c.hub().await {
        BrokerToHub::Error(e) => assert_eq!(e.code, ProtocolErrorCode::Malformed),
        other => panic!("expected refusal, got {other:?}"),
    }
    assert!(c.closed().await);
}
