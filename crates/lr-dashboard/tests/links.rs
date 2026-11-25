mod common;

use common::http::dashboard;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn status_of(addr: SocketAddr, path: &str) -> u16 {
    let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        addr.port()
    );
    tcp.write_all(request.as_bytes()).await.unwrap();
    let mut answer = String::new();
    tcp.read_to_string(&mut answer).await.unwrap();
    answer.split_whitespace().nth(1).unwrap().parse().unwrap()
}

fn token_of(url: &str) -> &str {
    url.split_once("?token=").unwrap().1
}

#[tokio::test(flavor = "multi_thread")]
async fn every_fresh_link_has_its_own_token_that_works_once() {
    let running = dashboard(common::source()).serve().await.unwrap();
    let addr = running.local_addr();
    let issuer = running.link_issuer();

    let first = issuer.fresh_url().unwrap();
    let second = issuer.fresh_url().unwrap();
    assert_ne!(token_of(&first), token_of(&second));
    for url in [&first, &second] {
        assert!(url.starts_with(&format!("http://127.0.0.1:{}/?token=", addr.port())));
        assert!(addr.ip().is_loopback());
    }

    let path = |url: &str| format!("/?token={}", token_of(url));
    assert_eq!(status_of(addr, &path(&first)).await, 303);
    assert_eq!(
        status_of(addr, &path(&first)).await,
        401,
        "a spent token is refused"
    );
    assert_eq!(
        status_of(addr, &path(&second)).await,
        303,
        "the other link is still good"
    );
    assert_eq!(status_of(addr, "/?token=wrong").await, 401);
    running.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_retires_the_first_link_and_old_links_expire_beyond_the_cap() {
    let running = dashboard(common::source()).serve().await.unwrap();
    let addr = running.local_addr();
    let issuer = running.link_issuer();
    let initial = running.url().to_string();
    issuer.revoke_all();
    assert_eq!(
        status_of(addr, &format!("/?token={}", token_of(&initial))).await,
        401
    );

    let urls: Vec<String> = (0..9).map(|_| issuer.fresh_url().unwrap()).collect();
    assert_eq!(
        status_of(addr, &format!("/?token={}", token_of(&urls[0]))).await,
        401,
        "the oldest unspent token is retired"
    );
    assert_eq!(
        status_of(addr, &format!("/?token={}", token_of(&urls[8]))).await,
        303
    );
    running.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_issuer_never_shows_a_token() {
    let running = dashboard(common::source()).serve().await.unwrap();
    let issuer = running.link_issuer();
    let url = issuer.fresh_url().unwrap();
    let shown = format!("{issuer:?}");
    assert!(!shown.contains(token_of(&url)), "{shown}");
    running.shutdown().await;
}
