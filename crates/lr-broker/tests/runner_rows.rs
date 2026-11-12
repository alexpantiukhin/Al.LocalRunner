mod common;

use common::harness::*;
use lr_abstractions::{BrokerStore, RunnerRow, RunnerState};
use lr_core::Zone;
use lr_proto::*;

async fn row(h: &Harness, name: &str) -> Option<RunnerRow> {
    h.store
        .runners()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.name == name)
}

async fn until_row_state(h: &Harness, name: &str, state: RunnerState) {
    for _ in 0..2000 {
        if row(h, name).await.map(|r| r.state) == Some(state) {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!(
        "row of {name} never became {state:?}, is {:?}",
        row(h, name).await.map(|r| r.state)
    );
}

#[tokio::test]
async fn a_local_runner_lives_in_session_memory_only() {
    let h = Harness::start("");
    let _local = h.local_runner("alex", "lr1", linux()).await;
    let _member = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.until_runner("local:lr1", true).await;
    let names: Vec<String> = h
        .store
        .runners()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(names, vec!["r1".to_string()]);
}

#[tokio::test]
async fn a_draining_row_stays_draining_through_heartbeats_until_bye() {
    let h = Harness::start("");
    let r = h.member_runner("r1", "10.0.0.7", linux()).await;
    until_row_state(&h, "r1", RunnerState::Online).await;
    r.send(&RunnerToBroker::Drain).await;
    until_row_state(&h, "r1", RunnerState::Draining).await;
    r.send(&RunnerToBroker::Heartbeat).await;
    let mut hub = h.local_hub("alex").await;
    hub.sync().await;
    assert_eq!(row(&h, "r1").await.unwrap().state, RunnerState::Draining);
    r.send(&RunnerToBroker::Bye(Bye { reason: None })).await;
    until_row_state(&h, "r1", RunnerState::Offline).await;
}

#[tokio::test]
async fn a_draining_row_ends_when_the_runner_connects_again() {
    let h = Harness::start("");
    let r = h.member_runner("r1", "10.0.0.7", linux()).await;
    r.send(&RunnerToBroker::Drain).await;
    until_row_state(&h, "r1", RunnerState::Draining).await;
    let mut again = h.dial(member_peer("pk-r1", "10.0.0.7")).await;
    again.send(&hello("r1", Some(linux()))).await;
    again.welcome().await;
    until_row_state(&h, "r1", RunnerState::Online).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    assert_eq!(again.assign().await.job.0, "j1");
}

#[tokio::test]
async fn after_a_restart_runners_are_offline_with_the_pool_as_capacity_and_remote() {
    let h = Harness::start("[networks]\nhome = [\"10.0.0.0/24\"]\n");
    let r = h
        .member_runner("r1", "10.0.0.7", caps(&["linux"], 4096, 2))
        .await;
    let mut before = h.member_hub("hub1", "", "10.0.0.5").await;
    before
        .send(&HubToBroker::CapabilitiesQuery(CapabilitiesQuery {
            req: 1,
        }))
        .await;
    let zone = loop {
        if let BrokerToHub::CapabilitiesReply(c) = before.hub().await {
            break c.runners[0].zone;
        }
    };
    assert_eq!(zone, Zone::Lan);
    r.close().await;

    let h2 = h.restart().await;
    let scans = h2.faults.seen_filters();
    assert!(
        scans
            .iter()
            .any(|f| f.active_only && f.limit == Some(100_000)),
        "{scans:?}"
    );
    let mut root = h2.local_hub("root").await;
    let ReadReply::Runners { runners } = root.read(ReadRequest::Runners).await else {
        panic!("runners")
    };
    let r1 = runners.iter().find(|x| x.name == "r1").expect("restored");
    assert!(!r1.online);
    assert_eq!((r1.caps.mem_mb, r1.caps.cpu), (16384, 8));
    let mut hub = h2.member_hub("hub2", "", "10.0.0.6").await;
    hub.send(&HubToBroker::CapabilitiesQuery(CapabilitiesQuery {
        req: 2,
    }))
    .await;
    let reply = loop {
        if let BrokerToHub::CapabilitiesReply(c) = hub.hub().await {
            break c;
        }
    };
    let restored = reply.runners.iter().find(|x| x.name == "r1").unwrap();
    assert_eq!(restored.zone, Zone::Remote);
    assert!(!restored.online);
}
