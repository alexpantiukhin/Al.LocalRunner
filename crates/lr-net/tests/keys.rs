mod common;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use common::*;
use lr_abstractions::{Dialer, Frame, MachineKind, TransportError};
use lr_core::Endpoint;
use lr_net::codec::{read_frame, write_frame};
use lr_net::{
    peer_kind, pinned_client_config, refusal_code, server_name, verify_challenge, BrokerIdentity,
    ClientKey, Fingerprint, NetError, TlsDialer, MAX_PENDING_HANDSHAKES,
};
use lr_proto::{
    decode, decode_for_peer, encode, AdminRequest, AuthFromBroker, AuthToBroker, BrokerToHub,
    DecodeError, HubToBroker, Join, PeerKind, ProtocolErrorCode, PROTOCOL_VERSION,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

#[tokio::test]
async fn signed_nonce_authenticates_key() {
    let b = broker().await;
    let k = key();
    b.register(&k, "light").await;
    let client = b.dial(&k).await.expect("registered key connects");
    let server = b.accept().await;
    assert_eq!(server.peer().key.as_deref(), Some(k.public_key().as_str()));
    assert_eq!(
        server.peer().endpoint,
        Endpoint::Addr("127.0.0.1".parse().unwrap())
    );
    assert_eq!(client.peer().key.as_deref(), Some(b.fingerprint().as_str()));
    exchange(client.as_ref(), server.as_ref()).await;
    exchange(server.as_ref(), client.as_ref()).await;
}

#[tokio::test]
async fn revoked_key_cannot_connect() {
    let b = broker().await;
    let (k, other) = (key(), key());
    b.register(&k, "light").await;
    b.register(&other, "heavy").await;
    b.dial(&k).await.expect("connects before revocation");
    b.accept().await;
    b.registry.revoke("light", at_min(5)).await.unwrap();
    let err = b.dial(&k).await.err().expect("revoked key refused");
    assert_eq!(refusal_code(&err), Some(ProtocolErrorCode::KeyRevoked));
    assert!(ProtocolErrorCode::KeyRevoked.kills_running_jobs());
    assert!(b.nothing_accepted().await, "the role never sees it");
    b.dial(&other).await.expect("another key is not affected");
}

#[test]
fn signature_for_one_nonce_rejected_for_another() {
    let k = ClientKey::generate().unwrap();
    let fp = BrokerIdentity::generate().unwrap().fingerprint();
    let (n1, n2) = ([1u8; 32], [2u8; 32]);
    let sig = k.sign_challenge(&fp, &n1);
    verify_challenge(&k.public_key(), &fp, &n1, &sig).expect("own nonce verifies");
    assert_eq!(
        verify_challenge(&k.public_key(), &fp, &n2, &sig),
        Err(NetError::BadSignature)
    );
    let other_broker = Fingerprint::from_bytes([9; 32]);
    assert_eq!(
        verify_challenge(&k.public_key(), &other_broker, &n1, &sig),
        Err(NetError::BadSignature),
        "a proof for one broker is useless at another"
    );
    let stranger = ClientKey::generate().unwrap();
    assert_eq!(
        verify_challenge(&stranger.public_key(), &fp, &n1, &sig),
        Err(NetError::BadSignature)
    );
}

async fn raw_tls(b: &Broker) -> TlsStream<TcpStream> {
    let config = pinned_client_config(b.identity.fingerprint()).unwrap();
    let tcp = TcpStream::connect(&b.addr).await.unwrap();
    TlsConnector::from(config)
        .connect(server_name(), tcp)
        .await
        .unwrap()
}

async fn challenge(tls: &mut TlsStream<TcpStream>) -> Vec<u8> {
    let msg: AuthFromBroker = decode(&read_frame(tls).await.unwrap()).unwrap();
    let AuthFromBroker::Challenge { nonce } = msg else {
        panic!("expected a challenge, got {msg:?}")
    };
    URL_SAFE_NO_PAD.decode(nonce).unwrap()
}

async fn send_proof(tls: &mut TlsStream<TcpStream>, proof: &AuthToBroker) -> AuthFromBroker {
    write_frame(tls, &encode(proof).unwrap()).await.unwrap();
    decode(&read_frame(tls).await.unwrap()).unwrap()
}

#[tokio::test]
async fn replayed_proof_refused() {
    let b = broker().await;
    let k = key();
    b.register(&k, "light").await;
    let fp = b.identity.fingerprint();

    let mut first = raw_tls(&b).await;
    let n1 = challenge(&mut first).await;
    let proof = AuthToBroker::Proof {
        public_key: k.public_key(),
        signature: k.sign_challenge(&fp, &n1),
    };
    assert_eq!(
        send_proof(&mut first, &proof).await,
        AuthFromBroker::Authenticated
    );

    let mut second = raw_tls(&b).await;
    let n2 = challenge(&mut second).await;
    assert_ne!(n1, n2, "every connection gets a fresh nonce");
    match send_proof(&mut second, &proof).await {
        AuthFromBroker::Error(e) => assert_eq!(e.code, ProtocolErrorCode::Unauthorized),
        other => panic!("replay accepted: {other:?}"),
    }
}

#[tokio::test]
async fn garbage_proof_refused() {
    let b = broker().await;
    let k = key();
    let mut tls = raw_tls(&b).await;
    challenge(&mut tls).await;
    let proof = AuthToBroker::Proof {
        public_key: k.public_key(),
        signature: "AAAA".into(),
    };
    assert!(matches!(
        send_proof(&mut tls, &proof).await,
        AuthFromBroker::Error(e) if e.code == ProtocolErrorCode::Unauthorized
    ));
    let mut tls = raw_tls(&b).await;
    challenge(&mut tls).await;
    let bad_key = AuthToBroker::Proof {
        public_key: "ed25519:not-a-key".into(),
        signature: "AAAA".into(),
    };
    assert!(matches!(
        send_proof(&mut tls, &bad_key).await,
        AuthFromBroker::Error(e) if e.code == ProtocolErrorCode::Unauthorized
    ));
}

#[tokio::test]
async fn oversized_proof_frame_refused_before_reading_it() {
    let b = broker().await;
    let mut tls = raw_tls(&b).await;
    challenge(&mut tls).await;
    use tokio::io::AsyncWriteExt;
    tls.write_all(&(1024u32 * 1024).to_be_bytes())
        .await
        .unwrap();
    tls.flush().await.unwrap();
    let end = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut tls)).await;
    assert!(matches!(end, Ok(Err(_))), "broker drops the connection");
    assert!(b.nothing_accepted().await);
}

#[tokio::test]
async fn wrong_fingerprint_refused() {
    let b = broker().await;
    let k = key();
    b.register(&k, "light").await;
    let impostor = BrokerIdentity::generate()
        .unwrap()
        .fingerprint()
        .to_string();
    let err = TlsDialer::new(k.clone())
        .dial(&b.addr, Some(&impostor))
        .await
        .err()
        .expect("mismatch refused");
    match &err {
        TransportError::Refused(why) => {
            assert!(why.starts_with("wrong_fingerprint"), "{why}");
            assert!(
                why.contains(&b.fingerprint()),
                "names what was presented: {why}"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(
        b.nothing_accepted().await,
        "nothing reaches the broker role"
    );
}

#[tokio::test]
async fn dial_without_fingerprint_or_with_garbage_refused() {
    let b = broker().await;
    let d = TlsDialer::new(key());
    assert!(matches!(
        d.dial(&b.addr, None).await.err(),
        Some(TransportError::Refused(_))
    ));
    assert!(matches!(
        d.dial(&b.addr, Some("sha256:zz")).await.err(),
        Some(TransportError::Refused(_))
    ));
}

#[tokio::test]
async fn nobody_listening_is_unreachable() {
    let b = broker().await;
    let addr = b.addr.clone();
    let fp = b.fingerprint();
    drop(b);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let err = TlsDialer::new(key()).dial(&addr, Some(&fp)).await.err();
    assert!(
        matches!(err, Some(TransportError::Unreachable(_))),
        "{err:?}"
    );
}

#[tokio::test]
async fn unknown_key_may_only_join() {
    let b = broker().await;
    let k = key();
    let client = b
        .dial(&k)
        .await
        .expect("unknown key gets a joining connection");
    let server = b.accept().await;
    client
        .send(Frame::json(&HubToBroker::Heartbeat).unwrap())
        .await
        .unwrap();
    let err = server.recv().await.expect_err("non-join refused");
    assert_eq!(refusal_code(&err), Some(ProtocolErrorCode::Unauthorized));
    let reply: BrokerToHub = client.recv().await.unwrap().parse().unwrap();
    assert!(matches!(reply, BrokerToHub::Error(e) if e.code == ProtocolErrorCode::Unauthorized));
}

#[tokio::test]
async fn join_over_the_network_registers_the_proven_key() {
    let b = broker().await;
    let code = b.issue(MachineKind::Hub, 1).await;
    let k = key();
    let client = b.dial(&k).await.unwrap();
    let server = b.accept().await;
    let join = Join {
        version: PROTOCOL_VERSION,
        role: lr_proto::Role::Hub,
        code: code.clone(),
        public_key: k.public_key(),
        name: "laptop".into(),
        machine_id: "m-91aa".into(),
        capabilities: None,
    };
    client
        .send(Frame::json(&HubToBroker::Join(join)).unwrap())
        .await
        .unwrap();
    let frame = server.recv().await.expect("a join passes the gate");
    let HubToBroker::Join(join) = frame.parse().unwrap() else {
        panic!("expected join")
    };
    let proven = server.peer().key.clone().unwrap();
    let mut req = join_request(&join.code, &join.name, &k);
    req.kind = MachineKind::Hub;
    req.machine_id = join.machine_id;
    let row = b.registry.redeem(req, &proven, at_min(1)).await.unwrap();
    assert_eq!(row.public_key, k.public_key());
    exchange(server.as_ref(), client.as_ref()).await;
    drop((client, server));
    b.dial(&k).await.expect("now a member");
    let member = b.accept().await;
    let heartbeat = Frame::json(&HubToBroker::Heartbeat).unwrap();
    let client2 = b.dial(&k).await.unwrap();
    let member2 = b.accept().await;
    client2.send(heartbeat).await.unwrap();
    member2
        .recv()
        .await
        .expect("members are not limited to join");
    drop(member);
}

#[tokio::test]
async fn admin_command_from_a_tls_peer_refused() {
    let b = broker().await;
    let k = key();
    b.register(&k, "light").await;
    let client = b.dial(&k).await.unwrap();
    let server = b.accept().await;
    assert_eq!(peer_kind(server.peer()), PeerKind::Network);
    let admin = AdminRequest::PolicyReload;
    client.send(Frame::json(&admin).unwrap()).await.unwrap();
    let frame = server.recv().await.unwrap();
    let err =
        decode_for_peer::<AdminRequest>(frame.as_bytes(), peer_kind(server.peer())).unwrap_err();
    assert!(matches!(err, DecodeError::NotOverNetwork(_)));
    let err =
        decode_for_peer::<HubToBroker>(frame.as_bytes(), peer_kind(server.peer())).unwrap_err();
    assert!(matches!(err, DecodeError::NotOverNetwork(_)));
}

#[tokio::test]
async fn stalled_handshakes_do_not_block_others() {
    let b = broker().await;
    let k = key();
    b.register(&k, "light").await;
    let mut idle = Vec::new();
    for _ in 0..(MAX_PENDING_HANDSHAKES / 2) {
        idle.push(TcpStream::connect(&b.addr).await.unwrap());
    }
    let started = std::time::Instant::now();
    b.dial(&k).await.expect("a real client still gets in");
    b.accept().await;
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(idle);
}

#[tokio::test]
async fn graceful_close_delivers_sent_frames_then_closed() {
    let b = broker().await;
    let k = key();
    b.register(&k, "light").await;
    let client = b.dial(&k).await.unwrap();
    let server = b.accept().await;
    for i in 0..3 {
        client
            .send(Frame::from_bytes(format!("{{\"n\":{i}}}").into_bytes()))
            .await
            .unwrap();
    }
    client.close().await;
    for i in 0..3 {
        let f = server.recv().await.unwrap();
        assert_eq!(f.as_bytes(), format!("{{\"n\":{i}}}").as_bytes());
    }
    assert_eq!(server.recv().await.err(), Some(TransportError::Closed));
    assert_eq!(
        client.send(Frame::from_bytes(b"{}".to_vec())).await.err(),
        Some(TransportError::Closed)
    );
}

#[test]
fn client_key_debug_hides_the_secret() {
    let dir = temp_dir();
    let k = ClientKey::generate().unwrap();
    let path = dir.path().join("hub.key");
    k.save(&path).unwrap();
    let secret_line = std::fs::read_to_string(&path).unwrap();
    let secret = secret_line.lines().nth(1).unwrap().to_string();
    let shown = format!("{k:?}");
    assert!(shown.contains(&k.public_key()));
    assert!(!shown.contains(&secret));
    let loaded = ClientKey::load(&path).unwrap();
    assert_eq!(loaded.public_key(), k.public_key());
    let _: Arc<ClientKey> = Arc::new(loaded);
}
