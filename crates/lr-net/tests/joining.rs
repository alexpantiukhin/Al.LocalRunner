mod common;

use common::*;
use lr_abstractions::{Frame, MachineKind, PeerAccess, Transport, TransportError};
use lr_net::{refusal_code, Limits, AUTH_FRAME_LIMIT};
use lr_proto::{BrokerToHub, HubToBroker, Join, ProtocolErrorCode, PROTOCOL_VERSION};
use std::time::Duration;

fn join_frame(code: &str, k: &lr_net::ClientKey) -> Frame {
    Frame::json(&HubToBroker::Join(Join {
        version: PROTOCOL_VERSION,
        role: lr_proto::Role::Hub,
        code: code.to_string(),
        public_key: k.public_key(),
        name: "laptop".into(),
        machine_id: "m-91aa".into(),
        capabilities: None,
    }))
    .unwrap()
}

async fn closed_soon(t: &dyn Transport) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(3), t.recv()).await,
        Ok(Err(_))
    )
}

#[tokio::test]
async fn unknown_key_without_open_code_refused_at_handshake() {
    let b = broker().await;
    let err = b.dial(&key()).await.err().expect("no code, no way in");
    assert_eq!(refusal_code(&err), Some(ProtocolErrorCode::JoinCodeUnknown));
    assert!(b.nothing_accepted().await, "the role never sees it");

    b.issue(MachineKind::Runner, 1).await;
    b.clock.advance(Duration::from_secs(61 * 60));
    let err = b
        .dial(&key())
        .await
        .err()
        .expect("expired code opens nothing");
    assert_eq!(refusal_code(&err), Some(ProtocolErrorCode::JoinCodeUnknown));

    let b = broker().await;
    b.register(&key(), "light").await;
    let err = b
        .dial(&key())
        .await
        .err()
        .expect("used-up code opens nothing");
    assert_eq!(refusal_code(&err), Some(ProtocolErrorCode::JoinCodeUnknown));
}

#[tokio::test]
async fn unknown_key_may_only_join() {
    let b = broker().await;
    b.issue(MachineKind::Hub, 1).await;
    let client = b.dial(&key()).await.expect("a code is open");
    let server = b.accept().await;
    assert_eq!(server.peer().access, PeerAccess::Joining);
    client
        .send(Frame::json(&HubToBroker::Heartbeat).unwrap())
        .await
        .unwrap();
    let err = server.recv().await.expect_err("non-join refused");
    assert_eq!(refusal_code(&err), Some(ProtocolErrorCode::Unauthorized));
    let reply: BrokerToHub = client.recv().await.unwrap().parse().unwrap();
    assert!(matches!(reply, BrokerToHub::Error(e) if e.code == ProtocolErrorCode::Unauthorized));
    assert!(closed_soon(client.as_ref()).await);
}

#[tokio::test]
async fn joining_connection_closes_after_the_reply() {
    let b = broker().await;
    let code = b.issue(MachineKind::Hub, 1).await;
    let k = key();
    let client = b.dial(&k).await.unwrap();
    let server = b.accept().await;
    assert!(server.peer().key.is_some(), "a key, but not a member");
    assert_eq!(server.peer().access, PeerAccess::Joining);
    client.send(join_frame(&code, &k)).await.unwrap();
    let frame = server.recv().await.expect("the join passes");
    assert!(matches!(frame.parse().unwrap(), HubToBroker::Join(_)));
    server
        .send(Frame::from_bytes(br#"{"type":"joined"}"#.to_vec()))
        .await
        .unwrap();
    assert_eq!(
        client.recv().await.unwrap().as_bytes(),
        br#"{"type":"joined"}"#
    );
    assert_eq!(client.recv().await.err(), Some(TransportError::Closed));
    assert_eq!(
        server.send(Frame::from_bytes(b"{}".to_vec())).await.err(),
        Some(TransportError::Closed),
        "one reply only"
    );
    assert_eq!(server.recv().await.err(), Some(TransportError::Closed));

    let mut req = join_request(&code, "laptop", &k);
    req.kind = MachineKind::Hub;
    let proven = server.peer().key.clone().unwrap();
    b.registry.redeem(req, &proven, at_min(1)).await.unwrap();
    b.dial(&k).await.expect("reconnects as a member");
    assert_eq!(b.accept().await.peer().access, PeerAccess::Member);
}

#[tokio::test]
async fn second_frame_of_a_joiner_never_reaches_the_role() {
    let b = broker().await;
    let code = b.issue(MachineKind::Hub, 1).await;
    let k = key();
    let client = b.dial(&k).await.unwrap();
    let server = b.accept().await;
    client.send(join_frame(&code, &k)).await.unwrap();
    client.send(join_frame(&code, &k)).await.unwrap();
    server.recv().await.expect("first join");
    assert_eq!(
        server.recv().await.err(),
        Some(TransportError::Closed),
        "a joiner is one request"
    );
}

#[tokio::test]
async fn oversized_join_frame_refused() {
    let b = broker().await;
    b.issue(MachineKind::Hub, 1).await;
    let client = b.dial(&key()).await.unwrap();
    let server = b.accept().await;
    let padding = "a".repeat(AUTH_FRAME_LIMIT);
    let big = format!(r#"{{"type":"join","pad":"{padding}"}}"#);
    client
        .send(Frame::from_bytes(big.into_bytes()))
        .await
        .unwrap();
    let err = server.recv().await.expect_err("over 64 KiB");
    assert!(matches!(err, TransportError::TooLarge { .. }), "{err:?}");
}

#[tokio::test]
async fn silent_joiner_is_dropped() {
    let b = broker_with(Limits {
        join_timeout: Duration::from_millis(300),
        ..Limits::default()
    })
    .await;
    b.issue(MachineKind::Hub, 1).await;
    let client = b.dial(&key()).await.unwrap();
    let server = b.accept().await;
    let got = tokio::time::timeout(Duration::from_secs(3), server.recv())
        .await
        .expect("the wait for a join ends");
    assert!(got.is_err());
    assert!(closed_soon(client.as_ref()).await);
}

#[tokio::test]
async fn members_are_not_limited_to_join() {
    let b = broker().await;
    let k = key();
    b.register(&k, "light").await;
    let client = b.dial(&k).await.unwrap();
    let server = b.accept().await;
    assert_eq!(server.peer().access, PeerAccess::Member);
    assert_eq!(client.peer().access, PeerAccess::PinnedBroker);
    for _ in 0..3 {
        exchange(client.as_ref(), server.as_ref()).await;
        exchange(server.as_ref(), client.as_ref()).await;
    }
    let padding = "a".repeat(AUTH_FRAME_LIMIT * 2);
    let big = format!(r#"{{"type":"heartbeat","pad":"{padding}"}}"#);
    client
        .send(Frame::from_bytes(big.into_bytes()))
        .await
        .unwrap();
    server.recv().await.expect("the full frame limit applies");
}
