mod common;

use common::harness::*;
use lr_abstractions::{BrokerStore, RunnerState};
use lr_broker::BusEvent;
use lr_core::Zone;
use lr_proto::*;
use std::time::Duration;

fn error(m: BrokerToHub) -> ProtocolError {
    match m {
        BrokerToHub::Error(e) => e,
        other => panic!("expected error, got {other:?}"),
    }
}

async fn runner_event(h: &mut Harness) -> QueueEvent {
    loop {
        let BusEvent { event, .. } = h.events.recv().await.unwrap();
        if matches!(
            event,
            QueueEvent::RunnerOnline { .. } | QueueEvent::RunnerOffline { .. }
        ) {
            return event;
        }
    }
}

#[tokio::test]
async fn runner_hello_registers_it_online() {
    let mut h = Harness::start("");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let row = h.store.runners().await.unwrap();
    assert_eq!(row.len(), 1);
    assert_eq!(row[0].state, RunnerState::Online);
    assert_eq!(row[0].machine_name, "r1");
    assert!(row[0].platforms.contains("linux"));
    assert_eq!(
        runner_event(&mut h).await,
        QueueEvent::RunnerOnline { name: "r1".into() }
    );
}

#[tokio::test]
async fn runner_disconnect_marks_it_offline() {
    let mut h = Harness::start("");
    let r = h.member_runner("r1", "10.0.0.7", linux()).await;
    runner_event(&mut h).await;
    r.close().await;
    assert_eq!(
        runner_event(&mut h).await,
        QueueEvent::RunnerOffline { name: "r1".into() }
    );
    assert_eq!(
        h.store.runners().await.unwrap()[0].state,
        RunnerState::Offline
    );
}

#[tokio::test]
async fn bye_marks_the_runner_offline() {
    let mut h = Harness::start("");
    let r = h.member_runner("r1", "10.0.0.7", linux()).await;
    runner_event(&mut h).await;
    r.send(&RunnerToBroker::Bye(Bye { reason: None })).await;
    assert_eq!(
        runner_event(&mut h).await,
        QueueEvent::RunnerOffline { name: "r1".into() }
    );
}

#[tokio::test]
async fn silent_connection_is_dropped_after_three_missed_heartbeats() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    h.clock.advance(Duration::from_secs(30));
    hub.send(&HubToBroker::Heartbeat).await;
    hub.until_runner("r1", true).await;
    h.clock.advance(Duration::from_secs(14));
    hub.until_runner("r1", true).await;
    h.clock.advance(Duration::from_secs(1));
    hub.until_runner("r1", false).await;
    assert!(r.closed().await);
    assert_eq!(
        h.store.runners().await.unwrap()[0].state,
        RunnerState::Offline
    );
}

#[tokio::test]
async fn heartbeat_keeps_the_runner_online() {
    let h = Harness::start("");
    let r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    for _ in 0..4 {
        h.clock.advance(Duration::from_secs(15));
        r.send(&RunnerToBroker::Heartbeat).await;
        hub.send(&HubToBroker::Heartbeat).await;
        hub.until_runner("r1", true).await;
    }
    let row = &h.store.runners().await.unwrap()[0];
    assert_eq!(row.state, RunnerState::Online);
    assert_eq!(row.last_seen, Some(lr_core::Timestamp(START + 60_000)));
}

#[tokio::test]
async fn broker_sends_heartbeat_every_15_s() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    h.clock.advance(Duration::from_secs(15));
    let v = r.raw().await.unwrap();
    assert_eq!(v["type"], "heartbeat");
    h.clock.advance(Duration::from_secs(15));
    let v = r.raw().await.unwrap();
    assert_eq!(v["type"], "heartbeat");
}

#[tokio::test]
async fn welcome_carries_heartbeat_interval_and_lost_kill_time() {
    let h = Harness::start("");
    let mut c = h.connect_local("alex").await;
    c.send(&hello("r1", Some(linux()))).await;
    let w = c.welcome().await;
    assert_eq!(w.heartbeat_s, 15);
    assert_eq!(w.broker_lost_kill_s, Some(180));
    assert_eq!(w.zone, Some(Zone::Local));
    let h = Harness::start("[grace]\nlost_min = 5\n");
    let mut c = h.connect_local("alex").await;
    c.send(&hello("r1", Some(linux()))).await;
    assert_eq!(c.welcome().await.broker_lost_kill_s, Some(300));
}

#[tokio::test]
async fn welcome_reports_the_zone_of_the_peer_address() {
    let h = Harness::start("[networks]\nhome = [\"10.0.0.0/24\"]\n");
    let key = h.seed("hub1", lr_abstractions::MachineKind::Hub, "").await;
    let mut lan = h.dial(member_peer(&key, "10.0.0.5")).await;
    lan.send(&hello("hub1", None)).await;
    assert_eq!(lan.welcome().await.zone, Some(Zone::Lan));
    let key = h.seed("hub2", lr_abstractions::MachineKind::Hub, "").await;
    let mut far = h.dial(member_peer(&key, "172.16.0.5")).await;
    far.send(&hello("hub2", None)).await;
    assert_eq!(far.welcome().await.zone, Some(Zone::Remote));
}

#[tokio::test]
async fn runner_name_held_by_another_machine_is_refused() {
    let h = Harness::start("");
    let _a = h.member_runner("r1", "10.0.0.7", linux()).await;
    let key = h
        .seed("other", lr_abstractions::MachineKind::Runner, "")
        .await;
    let mut b = h.dial(member_peer(&key, "10.0.0.8")).await;
    b.send(&hello("r1", Some(linux()))).await;
    assert_eq!(error(b.hub().await).code, ProtocolErrorCode::Unauthorized);
    assert!(b.closed().await);
}

#[tokio::test]
async fn incompatible_major_version_is_refused() {
    let h = Harness::start("");
    let mut c = h.connect_local("alex").await;
    c.send(&HubToBroker::Hello(Hello {
        version: Version { major: 2, minor: 0 },
        name: "hub".into(),
        capabilities: None,
    }))
    .await;
    let e = error(c.hub().await);
    assert_eq!(e.code, ProtocolErrorCode::UnsupportedVersion);
    assert_eq!(e.supported, Some(PROTOCOL_VERSION));
    assert!(c.closed().await);
}

#[tokio::test]
async fn local_user_not_in_the_list_is_refused() {
    let h = Harness::start("");
    let mut c = h.connect_local("eve").await;
    c.send(&hello("hub", None)).await;
    assert_eq!(error(c.hub().await).code, ProtocolErrorCode::Unauthorized);
    assert!(c.closed().await);
}

#[tokio::test]
async fn admin_message_over_the_network_is_refused() {
    let h = Harness::start("");
    let mut hub = h.member_hub("hub1", "", "10.0.0.5").await;
    hub.send_raw("{\"type\":\"policy_reload\"}").await;
    assert_eq!(
        error(hub.hub().await).code,
        ProtocolErrorCode::AdminOverNetwork
    );
}

#[tokio::test]
async fn first_message_must_be_hello() {
    let h = Harness::start("");
    let mut c = h.connect_local("alex").await;
    c.send(&HubToBroker::Heartbeat).await;
    assert_eq!(error(c.hub().await).code, ProtocolErrorCode::Malformed);
    assert!(c.closed().await);
}

#[tokio::test]
async fn hub_key_cannot_register_a_runner() {
    let h = Harness::start("");
    let key = h.seed("hub1", lr_abstractions::MachineKind::Hub, "").await;
    let mut c = h.dial(member_peer(&key, "10.0.0.5")).await;
    c.send(&hello("hub1", Some(linux()))).await;
    assert_eq!(error(c.hub().await).code, ProtocolErrorCode::Unauthorized);
    assert!(c.closed().await);
}

#[tokio::test]
async fn a_runner_key_registers_only_the_name_it_joined_with() {
    let h = Harness::start("");
    let key = h.seed("r1", lr_abstractions::MachineKind::Runner, "").await;
    let mut c = h.dial(member_peer(&key, "10.0.0.5")).await;
    c.send(&hello("r2", Some(linux()))).await;
    assert_eq!(error(c.hub().await).code, ProtocolErrorCode::Unauthorized);
    assert!(c.closed().await);
    assert!(h
        .store
        .runners()
        .await
        .unwrap()
        .iter()
        .all(|r| r.name != "r2"));
}

#[tokio::test]
async fn joining_connection_is_refused_without_a_registry() {
    let h = Harness::start("");
    let mut c = h
        .dial(lr_abstractions::Peer {
            endpoint: lr_core::Endpoint::Addr(ip("10.0.0.5")),
            key: Some("fresh".into()),
            access: lr_abstractions::PeerAccess::Joining,
        })
        .await;
    c.send(&hello("x", None)).await;
    assert_eq!(error(c.hub().await).code, ProtocolErrorCode::Unauthorized);
    assert!(c.closed().await);
}

#[tokio::test]
async fn runner_reconnect_replaces_the_old_connection() {
    let h = Harness::start("");
    let mut old = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut new = h.dial(member_peer("pk-r1", "10.0.0.7")).await;
    new.send(&hello("r1", Some(linux()))).await;
    new.welcome().await;
    assert!(old.closed().await);
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("r1", true).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    assert_eq!(new.assign().await.job, lr_core::JobId("j1".into()));
    assert_eq!(
        h.store.runners().await.unwrap()[0].state,
        RunnerState::Online
    );
}

#[tokio::test]
async fn hub_heartbeat_updates_the_machine_last_seen() {
    let h = Harness::start("");
    let mut hub = h.member_hub("hub1", "", "10.0.0.5").await;
    h.clock.advance(Duration::from_secs(15));
    hub.send(&HubToBroker::Heartbeat).await;
    hub.sync().await;
    let m = h.store.machine_by_key("pk-hub1").await.unwrap().unwrap();
    assert_eq!(m.last_seen, Some(lr_core::Timestamp(START + 15_000)));
}

#[tokio::test]
async fn hub_bye_closes_the_connection() {
    let h = Harness::start("");
    let mut hub = h.local_hub("alex").await;
    hub.send(&HubToBroker::Bye(Bye { reason: None })).await;
    assert!(hub.closed().await);
}

#[tokio::test]
async fn unknown_message_type_of_a_newer_minor_version_is_skipped() {
    let h = Harness::start("");
    let mut hub = h.local_hub("alex").await;
    hub.send_raw("{\"type\":\"future_message\",\"x\":1}").await;
    let before = hub.sync().await;
    assert!(before.is_empty(), "{before:?}");
}
