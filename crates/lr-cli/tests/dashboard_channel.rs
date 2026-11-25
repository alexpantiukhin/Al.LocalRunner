mod common;

use common::loopback::layout;
use common::TempDir;
use lr_cli::dashboard::HubSource;
use lr_cli::dashboard_channel::{find_running, listen, request_link, serve, stop_running};
use lr_dashboard::{Dashboard, DashboardOptions};
use lr_proto::DashboardReply;
use std::sync::Arc;
use tokio::sync::Notify;

fn token_of(url: &str) -> &str {
    url.split_once("?token=").unwrap().1
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_answers_without_a_dashboard() {
    let dir = TempDir::new("dashboard-channel-none");
    let layout = layout(&dir);
    assert!(find_running(&layout).await.is_none());
    assert!(!stop_running(&layout).await);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_socket_left_by_a_dead_process_is_not_a_running_dashboard() {
    let dir = TempDir::new("dashboard-channel-stale");
    let layout = layout(&dir);
    layout.prepare_own_ipc().unwrap();
    let principal = lr_net::current_os_principal().unwrap();
    let path = layout.dashboard_ipc(&principal);
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    assert!(std::path::Path::new(&path).exists());
    assert!(find_running(&layout).await.is_none());
    let listener = listen(&layout)
        .await
        .expect("a new dashboard replaces the stale socket");
    listener.closed().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_repeated_call_gets_a_fresh_one_time_link_on_loopback_and_stop_ends_it() {
    let dir = TempDir::new("dashboard-channel-links");
    let layout = layout(&dir);
    let user = lr_net::current_os_user().unwrap();
    let source = Arc::new(HubSource::new(layout.clone()));
    let running = Dashboard::new(source, DashboardOptions { user })
        .unwrap()
        .serve()
        .await
        .unwrap();
    let issuer = running.link_issuer();
    issuer.revoke_all();
    let listener = Arc::new(listen(&layout).await.unwrap());
    let stop = Arc::new(Notify::new());
    let channel = tokio::spawn(serve(listener.clone(), issuer, stop.clone()));

    let second = listen(&layout).await;
    assert!(second.is_err(), "one dashboard per user");

    let mut urls = Vec::new();
    for _ in 0..2 {
        let client = find_running(&layout).await.expect("the dashboard answers");
        urls.push(request_link(&client).await.unwrap());
        client.close().await;
    }
    assert_ne!(token_of(&urls[0]), token_of(&urls[1]));
    for url in &urls {
        assert!(
            url.starts_with(&format!(
                "http://127.0.0.1:{}/?token=",
                running.local_addr().port()
            )),
            "{url}"
        );
        assert_eq!(token_of(url).len(), 64);
    }
    let shown = format!(
        "{:?}",
        DashboardReply::Link {
            url: urls[0].clone()
        }
    );
    assert!(!shown.contains(token_of(&urls[0])), "{shown}");

    assert!(stop_running(&layout).await);
    stop.notified().await;
    listener.closed().await;
    channel.abort();
    running.shutdown().await;
    assert!(find_running(&layout).await.is_none());
}

fn source_of(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn the_token_goes_to_the_terminal_and_never_to_a_log() {
    let channel = source_of("dashboard_channel.rs");
    for banned in ["println!", "eprintln!", "diag(", "tracing::", "log::"] {
        assert!(
            !channel.contains(banned),
            "dashboard_channel.rs uses {banned}"
        );
    }
    for line in source_of("dashboard.rs")
        .lines()
        .filter(|l| l.contains("eprintln!") || l.contains("diag(") || l.contains("tracing::"))
    {
        for secret in ["url", "link", "token"] {
            assert!(!line.contains(secret), "{line}");
        }
    }
}
