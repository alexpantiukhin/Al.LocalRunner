mod common;

use common::*;
use lr_net::{Limits, HANDSHAKE_TIMEOUT, MAX_PENDING_HANDSHAKES};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpSocket, TcpStream};

async fn junk_from(source: &str, broker: &str) -> TcpStream {
    let socket = TcpSocket::new_v4().unwrap();
    socket.bind(format!("{source}:0").parse().unwrap()).unwrap();
    socket.connect(broker.parse().unwrap()).await.unwrap()
}

async fn dropped_within(s: &mut TcpStream, wait: Duration) -> bool {
    let mut buf = [0u8; 64];
    matches!(
        tokio::time::timeout(wait, s.read(&mut buf)).await,
        Ok(Ok(0) | Err(_))
    )
}

fn small(general: usize, reserved: usize, per_ip: usize) -> Limits {
    Limits {
        general_slots: general,
        reserved_slots: reserved,
        per_ip,
        handshake_timeout: Duration::from_secs(20),
        ..Limits::default()
    }
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[test]
fn default_limits_split_the_handshake_cap() {
    let l = Limits::default();
    assert_eq!(l.general_slots + l.reserved_slots, MAX_PENDING_HANDSHAKES);
    assert!(l.reserved_slots > 0 && l.per_ip < l.general_slots);
    assert_eq!(l.handshake_timeout, HANDSHAKE_TIMEOUT);
}

#[tokio::test]
async fn one_address_cannot_take_more_than_its_share() {
    let b = broker_with(small(8, 0, 2)).await;
    let k = key();
    b.register(&k, "light").await;
    let mut held = vec![
        junk_from("127.0.0.2", &b.addr).await,
        junk_from("127.0.0.2", &b.addr).await,
    ];
    settle().await;
    let mut third = junk_from("127.0.0.2", &b.addr).await;
    assert!(
        dropped_within(&mut third, Duration::from_secs(2)).await,
        "over the per-address limit"
    );
    for s in &mut held {
        assert!(!dropped_within(s, Duration::from_millis(200)).await);
    }
    b.dial(&k).await.expect("another address still gets in");
    b.accept().await;
}

#[tokio::test]
async fn full_slots_drop_newcomers_but_not_known_members() {
    let b = broker_with(small(4, 2, 8)).await;
    let k = key();
    b.register(&k, "light").await;
    b.dial(&k).await.expect("first connection");
    b.accept().await;
    let mut junk = Vec::new();
    for i in 2..6 {
        junk.push(junk_from(&format!("127.0.0.{i}"), &b.addr).await);
    }
    settle().await;
    let mut late = junk_from("127.0.0.9", &b.addr).await;
    assert!(
        dropped_within(&mut late, Duration::from_secs(2)).await,
        "all general slots are taken"
    );
    let started = std::time::Instant::now();
    b.dial(&k)
        .await
        .expect("a member reconnecting from its address gets a reserved slot");
    b.accept().await;
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(junk);
}

#[tokio::test]
async fn reserved_slots_are_only_for_known_addresses() {
    let b = broker_with(small(4, 2, 8)).await;
    let k = key();
    b.register(&k, "light").await;
    let mut junk = Vec::new();
    for i in 2..6 {
        junk.push(junk_from(&format!("127.0.0.{i}"), &b.addr).await);
    }
    settle().await;
    assert!(
        b.dial(&k).await.is_err(),
        "an address that never authenticated has no reserved slot"
    );
    drop(junk);
}

#[tokio::test]
async fn junk_from_a_member_address_cannot_lock_it_out() {
    let b = broker_with(small(8, 2, 3)).await;
    let k = key();
    b.register(&k, "light").await;
    b.dial(&k).await.unwrap();
    b.accept().await;
    let mut junk = Vec::new();
    for _ in 0..3 {
        junk.push(junk_from("127.0.0.1", &b.addr).await);
    }
    settle().await;
    b.dial(&k)
        .await
        .expect("over its per-address share a known address uses the reserved pool");
    b.accept().await;
    drop(junk);
}

#[tokio::test]
async fn handshake_timeout_drops_idle_connections_and_frees_the_slot() {
    let b = broker_with(Limits {
        general_slots: 1,
        reserved_slots: 0,
        per_ip: 1,
        handshake_timeout: Duration::from_millis(400),
        ..Limits::default()
    })
    .await;
    let k = key();
    b.register(&k, "light").await;
    let mut idle = junk_from("127.0.0.2", &b.addr).await;
    assert!(
        dropped_within(&mut idle, Duration::from_secs(3)).await,
        "the broker closes a connection that never finishes the handshake"
    );
    b.dial(&k).await.expect("the slot is free again");
    b.accept().await;
}
