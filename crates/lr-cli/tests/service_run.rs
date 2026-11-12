mod common;

use common::TempDir;
use lr_cli::os_service::service_run::{serve, service_log, ServiceEnd};
use lr_cli::paths::Layout;
use std::cell::Cell;

fn layout(dir: &TempDir, policy: &str) -> Layout {
    let root = dir.path().join("state");
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("policy.toml"), policy).unwrap();
    Layout::new(root, config)
}

fn log_lines(layout: &Layout) -> Vec<(u64, String)> {
    let text =
        std::fs::read_to_string(layout.root.join("logs").join("service.log")).unwrap_or_default();
    text.lines()
        .map(|l| {
            let (at, rest) = l.split_once(' ').expect("time and text");
            (at.parse().expect("unix seconds"), rest.to_string())
        })
        .collect()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broker_that_cannot_start_fails_the_service_and_says_why_in_service_log() {
    let dir = TempDir::new("svc-bad");
    let layout = layout(&dir, "[listen\naddress = ");
    let started = Cell::new(false);
    let before = now();
    let end = serve(&layout, || started.set(true), std::future::pending()).await;
    assert_eq!(end, ServiceEnd::NotStarted);
    assert!(!started.get());
    let lines = log_lines(&layout);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let (at, text) = &lines[0];
    assert!(*at >= before && *at <= now(), "{at}");
    assert!(
        text.starts_with("the broker did not start: ") && text.contains("policy.toml"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_service_stopped_by_its_manager_ends_cleanly_and_logs_nothing() {
    let dir = TempDir::new("svc-ok");
    let user = lr_net::current_os_user().unwrap();
    let layout = layout(&dir, &format!("[[local_user]]\nname = {user:?}\n"));
    let started = Cell::new(false);
    let end = serve(&layout, || started.set(true), async {}).await;
    assert_eq!(end, ServiceEnd::Stopped);
    assert!(started.get());
    assert!(log_lines(&layout).is_empty());
}

#[test]
fn service_log_appends_one_timestamped_line_per_call() {
    let dir = TempDir::new("svc-log");
    let layout = layout(&dir, "");
    service_log(&layout, "first");
    service_log(&layout, "second reason\n  |\n  detail\n");
    let texts: Vec<String> = log_lines(&layout).into_iter().map(|(_, t)| t).collect();
    assert_eq!(texts, ["first", "second reason; |; detail"]);
}
