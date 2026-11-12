mod common;

use common::*;
use lr_abstractions::{MachineKind, Transport};
use lr_net::{refusal_code, Limits};
use lr_proto::{AuthFromBroker, ProtocolErrorCode};
use std::time::Duration;

async fn closed_within(t: &dyn Transport, wait: Duration) -> bool {
    matches!(tokio::time::timeout(wait, t.recv()).await, Ok(Err(_)))
}

#[tokio::test]
async fn pending_handovers_keep_their_handshake_slots() {
    let b = broker_with(Limits {
        general_slots: 2,
        reserved_slots: 0,
        handover_timeout: Duration::from_secs(3),
        ..Limits::default()
    })
    .await;
    let k = key();
    b.register(&k, "light").await;
    let queued = b.dial(&k).await.expect("waits in the ready queue");
    let first = b.dial(&k).await.expect("waits for the role");
    let second = b.dial(&k).await.expect("waits for the role");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        b.dial(&k).await.is_err(),
        "transports the role has not taken still hold their slots"
    );
    assert!(closed_within(first.as_ref(), Duration::from_secs(6)).await);
    assert!(closed_within(second.as_ref(), Duration::from_secs(1)).await);
    b.dial(&k)
        .await
        .expect("slots are free after the hand-over timeout");
    drop(queued);
}

#[tokio::test]
async fn transports_the_role_never_takes_are_closed() {
    let b = broker_with(Limits {
        handover_timeout: Duration::from_millis(300),
        ..Limits::default()
    })
    .await;
    let k = key();
    b.register(&k, "light").await;
    let _queued = b.dial(&k).await.unwrap();
    for _ in 0..6 {
        let client = b.dial(&k).await.expect("the proof succeeds");
        assert!(
            closed_within(client.as_ref(), Duration::from_secs(3)).await,
            "no task or socket waits forever for a slow accept()"
        );
    }
}

fn joining_limits() -> Limits {
    Limits {
        joining_slots: 3,
        joining_per_ip: 2,
        ..Limits::default()
    }
}

#[tokio::test]
async fn joining_connections_are_capped_per_source() {
    let b = broker_with(joining_limits()).await;
    b.issue(MachineKind::Runner, 5).await;
    let _one = b.dial(&key()).await.expect("first joiner");
    let s1 = b.accept().await;
    let _two = b.dial(&key()).await.expect("second joiner");
    let _s2 = b.accept().await;
    let err = b.dial(&key()).await.err().expect("third from one source");
    assert_eq!(refusal_code(&err), Some(ProtocolErrorCode::RateLimited));
    s1.close().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    b.dial(&key())
        .await
        .expect("a closed joiner gives its slot back");
}

#[tokio::test]
async fn joining_connections_are_capped_globally() {
    let b = broker_with(joining_limits()).await;
    b.issue(MachineKind::Runner, 5).await;
    let mut held = Vec::new();
    for source in ["127.0.0.2", "127.0.0.3", "127.0.0.4"] {
        let (stream, verdict) = prove_from(&b, source, &key()).await;
        assert_eq!(verdict, Some(AuthFromBroker::Authenticated), "{source}");
        held.push((stream, b.accept().await));
    }
    let (_s, verdict) = prove_from(&b, "127.0.0.5", &key()).await;
    match verdict {
        Some(AuthFromBroker::Error(e)) => assert_eq!(e.code, ProtocolErrorCode::RateLimited),
        other => panic!("expected rate_limited, got {other:?}"),
    }
    let k = key();
    b.register(&k, "light").await;
    b.dial(&k)
        .await
        .expect("members are not counted as joiners");
}

#[tokio::test]
async fn idle_joiner_is_shut_down_at_the_end_of_its_lifetime() {
    let b = broker_with(Limits {
        joining_slots: 1,
        joining_lifetime: Duration::from_millis(400),
        join_timeout: Duration::from_secs(30),
        ..Limits::default()
    })
    .await;
    b.issue(MachineKind::Runner, 5).await;
    let client = b.dial(&key()).await.unwrap();
    let _server = b.accept().await;
    assert!(
        closed_within(client.as_ref(), Duration::from_secs(3)).await,
        "the role never reads, the transport still ends it"
    );
    b.dial(&key())
        .await
        .expect("the slot was given back although the role kept the transport");
}
