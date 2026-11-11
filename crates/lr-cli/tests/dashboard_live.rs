mod common;

use common::loopback::{layout, place, repo, until_ready};
use common::TempDir;
use lr_cli::dashboard::HubSource;
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_dashboard::{Dashboard, DashboardOptions};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn request(addr: &str, path: &str, cookie: Option<&str>, until: Option<&str>) -> String {
    let mut s = TcpStream::connect(addr)
        .await
        .expect("the dashboard listens");
    let cookie = cookie
        .map(|c| format!("Cookie: {c}\r\n"))
        .unwrap_or_default();
    let head = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n{cookie}Connection: close\r\n\r\n");
    s.write_all(head.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    let read = async {
        loop {
            let n = s.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            if until.is_some_and(|u| String::from_utf8_lossy(&out).contains(u)) {
                break;
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(10), read).await;
    String::from_utf8_lossy(&out).into_owned()
}

fn status(response: &str) -> u16 {
    response
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_live_dashboard_streams_events_to_a_session_and_refuses_one_without_the_token() {
    let dir = TempDir::new("dashboard-live");
    let layout = layout(&dir);
    let repo = repo(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    until_ready(&layout).await;

    let source = Arc::new(HubSource::new(layout.clone()));
    let user = lr_net::current_os_user().unwrap();
    let running = Dashboard::new(source, DashboardOptions { user })
        .unwrap()
        .serve()
        .await
        .expect("the dashboard serves");
    let url = running.url().to_string();
    let rest = url.strip_prefix("http://").unwrap();
    let (addr, query) = rest.split_once('/').unwrap();

    let anonymous = request(addr, "/api/events", None, Some("\r\n\r\n")).await;
    assert!(
        matches!(status(&anonymous), 401 | 403),
        "no session, no stream: {anonymous}"
    );
    let wrong = request(addr, "/?token=0000", None, Some("\r\n\r\n")).await;
    assert!(matches!(status(&wrong), 401 | 403), "{wrong}");

    let login = request(addr, &format!("/{query}"), None, Some("\r\n\r\n")).await;
    assert_eq!(status(&login), 303, "{login}");
    let cookie = login
        .lines()
        .find_map(|l| {
            l.strip_prefix("set-cookie: ")
                .or_else(|| l.strip_prefix("Set-Cookie: "))
        })
        .and_then(|c| c.split(';').next())
        .expect("the token gives a session cookie")
        .to_string();

    let job = place(
        &layout,
        common::session("agent", Some("live"), &repo).unwrap(),
    )
    .await;
    let snapshot = request(addr, "/api/snapshot", Some(&cookie), None).await;
    assert_eq!(status(&snapshot), 200, "{snapshot}");
    assert!(
        snapshot.contains(&job.0),
        "the new job is in the data: {snapshot}"
    );

    let events = request(addr, "/api/events", Some(&cookie), Some("data:")).await;
    assert_eq!(status(&events), 200, "{events}");
    assert!(
        events.to_ascii_lowercase().contains("text/event-stream"),
        "{events}"
    );
    assert!(events.contains("data:"), "an event arrives: {events}");

    running.shutdown().await;
    runners.stop();
    hub_role.stop();
    broker.stop();
}
