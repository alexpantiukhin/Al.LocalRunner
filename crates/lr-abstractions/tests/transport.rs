mod common;

use common::{clock, ipc_peer, net_peer, settle};
use lr_abstractions::fakes::{channel_pair, FakeNetwork};
use lr_abstractions::{Dialer, Frame, Listener, Transport, TransportError, MAX_FRAME_BYTES};
use lr_core::Endpoint;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Ping {
    n: u32,
}

#[tokio::test]
async fn frames_flow_both_ways_in_order_as_json() {
    let (_, clock) = clock();
    let (a, b) = channel_pair(clock, ipc_peer("a"), ipc_peer("b"));
    for n in 0..3 {
        a.send(Frame::json(&Ping { n }).unwrap()).await.unwrap();
    }
    b.send(Frame::json(&Ping { n: 9 }).unwrap()).await.unwrap();
    for n in 0..3 {
        assert_eq!(b.recv().await.unwrap().parse::<Ping>().unwrap(), Ping { n });
    }
    assert_eq!(
        a.recv().await.unwrap().parse::<Ping>().unwrap(),
        Ping { n: 9 }
    );
}

#[tokio::test]
async fn each_end_sees_the_other_peer_identity_and_address() {
    let (_, clock) = clock();
    let hub = net_peer("192.168.1.20", "hub-key");
    let broker = net_peer("192.168.1.2", "broker-fp");
    let (at_hub, at_broker) = channel_pair(clock, hub.clone(), broker.clone());
    assert_eq!(at_hub.peer(), &broker);
    assert_eq!(at_broker.peer(), &hub);
    assert_eq!(
        at_broker.peer().endpoint,
        Endpoint::Addr("192.168.1.20".parse().unwrap())
    );
}

#[tokio::test]
async fn frame_over_4_mib_is_refused_and_not_delivered() {
    let (_, clock) = clock();
    let (a, b) = channel_pair(clock, ipc_peer("a"), ipc_peer("b"));
    let big = Frame::from_bytes(vec![b'x'; MAX_FRAME_BYTES + 1]);
    assert!(matches!(
        a.send(big).await,
        Err(TransportError::TooLarge { .. })
    ));
    a.send(Frame::from_bytes(vec![b'y'; MAX_FRAME_BYTES]))
        .await
        .unwrap();
    assert_eq!(b.recv().await.unwrap().len(), MAX_FRAME_BYTES);
}

#[tokio::test]
async fn disconnect_closes_both_ends_and_loses_frames_in_flight() {
    let (_, clock) = clock();
    let (a, b) = channel_pair(clock, ipc_peer("a"), ipc_peer("b"));
    a.send(Frame::from_bytes(b"lost".to_vec())).await.unwrap();
    a.faults().disconnect();
    assert!(matches!(b.recv().await, Err(TransportError::Closed)));
    assert!(matches!(
        b.send(Frame::from_bytes(b"x".to_vec())).await,
        Err(TransportError::Closed)
    ));
    assert!(matches!(a.recv().await, Err(TransportError::Closed)));
}

#[tokio::test]
async fn close_by_one_end_is_seen_by_the_other() {
    let (_, clock) = clock();
    let (a, b) = channel_pair(clock, ipc_peer("a"), ipc_peer("b"));
    a.close().await;
    assert!(matches!(b.recv().await, Err(TransportError::Closed)));
}

#[tokio::test]
async fn graceful_close_delivers_frames_sent_before_it_then_closed() {
    let (_, clock) = clock();
    let (a, b) = channel_pair(clock, ipc_peer("a"), ipc_peer("b"));
    a.send(Frame::from_bytes(b"Bye".to_vec())).await.unwrap();
    a.close().await;
    assert!(matches!(
        a.send(Frame::from_bytes(b"late".to_vec())).await,
        Err(TransportError::Closed)
    ));
    assert!(matches!(a.recv().await, Err(TransportError::Closed)));
    assert_eq!(b.recv().await.unwrap().into_bytes(), b"Bye".to_vec());
    assert!(matches!(b.recv().await, Err(TransportError::Closed)));
    assert!(matches!(
        b.send(Frame::from_bytes(b"x".to_vec())).await,
        Err(TransportError::Closed)
    ));
}

#[tokio::test]
async fn delayed_frame_arrives_only_after_clock_advance_and_keeps_order() {
    let (manual, clock) = clock();
    let (a, b) = channel_pair(clock, ipc_peer("a"), ipc_peer("b"));
    a.faults().delay_next(1, Duration::from_secs(2));
    a.send(Frame::from_bytes(b"1".to_vec())).await.unwrap();
    a.send(Frame::from_bytes(b"2".to_vec())).await.unwrap();
    let task = tokio::spawn(async move {
        let first = b.recv().await.unwrap().into_bytes();
        let second = b.recv().await.unwrap().into_bytes();
        (first, second)
    });
    manual.wait_for_sleepers(1).await;
    settle().await;
    assert!(!task.is_finished());
    manual.advance(Duration::from_secs(2));
    assert_eq!(task.await.unwrap(), (b"1".to_vec(), b"2".to_vec()));
}

#[tokio::test]
async fn listener_accepts_dialed_connection_with_client_peer() {
    let (_, clock) = clock();
    let net = FakeNetwork::new(clock);
    let broker = net_peer("10.0.0.1", "broker-fp");
    let listener = net.listen("broker:7000", broker.clone());
    let client = net_peer("10.0.0.5", "hub-key");
    let dialer = net.dialer(client.clone());
    let at_hub = dialer.dial("broker:7000", Some("broker-fp")).await.unwrap();
    let at_broker = listener.accept().await.unwrap();
    assert_eq!(at_hub.peer(), &broker);
    assert_eq!(at_broker.peer(), &client);
    at_hub
        .send(Frame::from_bytes(b"hi".to_vec()))
        .await
        .unwrap();
    assert_eq!(at_broker.recv().await.unwrap().into_bytes(), b"hi".to_vec());
}

#[tokio::test]
async fn unreachable_or_unknown_address_refuses_dial() {
    let (_, clock) = clock();
    let net = FakeNetwork::new(clock);
    let _listener = net.listen("broker:7000", net_peer("10.0.0.1", "fp"));
    let dialer = net.dialer(net_peer("10.0.0.5", "k"));
    assert!(matches!(
        dialer.dial("nowhere:1", None).await,
        Err(TransportError::Unreachable(_))
    ));
    net.set_reachable("broker:7000", false);
    assert!(matches!(
        dialer.dial("broker:7000", None).await,
        Err(TransportError::Unreachable(_))
    ));
    net.set_reachable("broker:7000", true);
    assert!(dialer.dial("broker:7000", None).await.is_ok());
}

#[tokio::test]
async fn dial_with_wrong_fingerprint_is_refused() {
    let (_, clock) = clock();
    let net = FakeNetwork::new(clock);
    let listener = net.listen("broker:7000", net_peer("10.0.0.1", "real-fp"));
    let dialer = net.dialer(net_peer("10.0.0.5", "k"));
    assert!(matches!(
        dialer.dial("broker:7000", Some("forged-fp")).await,
        Err(TransportError::Refused(_))
    ));
    assert_eq!(listener.pending(), 0);
}

fn bytes(s: &str) -> Frame {
    Frame::from_bytes(s.as_bytes().to_vec())
}

#[tokio::test]
async fn faults_reach_connections_made_through_the_network() {
    let (manual, clock) = clock();
    let net = FakeNetwork::new(clock);
    let listener = net.listen("broker:7000", net_peer("10.0.0.1", "fp"));
    let at_hub = net
        .dialer(net_peer("10.0.0.5", "k"))
        .dial("broker:7000", None)
        .await
        .unwrap();
    let at_broker = listener.accept().await.unwrap();
    let conn = net.connections("broker:7000").remove(0);
    conn.client.duplicate_next(1);
    at_hub.send(bytes("order")).await.unwrap();
    at_hub.send(bytes("next")).await.unwrap();
    for want in ["order", "order", "next"] {
        assert_eq!(at_broker.recv().await.unwrap(), bytes(want));
    }
    conn.server.drop_next(1);
    at_broker.send(bytes("lost")).await.unwrap();
    at_broker.send(bytes("kept")).await.unwrap();
    assert_eq!(at_hub.recv().await.unwrap(), bytes("kept"));
    conn.server.delay_next(1, Duration::from_secs(5));
    at_broker.send(bytes("late")).await.unwrap();
    let task = tokio::spawn(async move { at_hub.recv().await.unwrap() });
    manual.wait_for_sleepers(1).await;
    assert!(!task.is_finished());
    manual.advance(Duration::from_secs(5));
    assert_eq!(task.await.unwrap(), bytes("late"));
}

#[tokio::test]
async fn cut_drops_live_connections_to_address() {
    let (_, clock) = clock();
    let net = FakeNetwork::new(clock);
    let listener = net.listen("broker:7000", net_peer("10.0.0.1", "fp"));
    let at_hub = net
        .dialer(net_peer("10.0.0.5", "k"))
        .dial("broker:7000", None)
        .await
        .unwrap();
    let at_broker = listener.accept().await.unwrap();
    assert_eq!(net.connections("broker:7000").len(), 1);
    net.cut("broker:7000");
    assert!(net.connections("broker:7000").is_empty());
    assert!(matches!(at_hub.recv().await, Err(TransportError::Closed)));
    assert!(matches!(
        at_broker.recv().await,
        Err(TransportError::Closed)
    ));
}
