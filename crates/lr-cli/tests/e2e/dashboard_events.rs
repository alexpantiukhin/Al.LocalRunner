use crate::common::TempDir;
use crate::support::*;
use lr_cli::commands::jobs::hub;
use lr_cli::commands::join_cmd::hub_join;
use lr_cli::commands::view::read;
use lr_cli::dashboard::HubSource;
use lr_cli::ipc_client::IpcClient;
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_core::{JobId, JobState};
use lr_dashboard::{Dashboard, DashboardOptions, RunningDashboard};
use lr_proto::{AdminReply, AdminRequest, ReadRequest};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const QUICK: &str =
    "[profile.quick]\ncommand = \"echo e2e-event\"\ntimeout_s = 120\nmem_mb = 256\n";

async fn hub_code(layout: &Layout) -> (String, String) {
    let admin = IpcClient::connect_broker_admin(&layout.admin_ipc())
        .await
        .expect("admin channel");
    let req = AdminRequest::BrokerAdd {
        name: Some("guest".into()),
        label: None,
        runners: None,
        address: None,
    };
    let reply: AdminReply = admin.call(&req).await.expect("admin reply");
    admin.close().await;
    match reply {
        AdminReply::JoinCode { address, code, .. } => (address, code),
        other => panic!("broker add answered {other:?}"),
    }
}

async fn http(addr: &str, path: &str) -> String {
    let mut s = TcpStream::connect(addr).await.expect("dashboard listens");
    let head = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    s.write_all(head.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).into_owned()
}

/// A dashboard over the hub of a layout and one SSE connection of a session to it, whose
/// `update` events are collected as they arrive.
struct Watched {
    running: RunningDashboard,
    events: Arc<Mutex<Vec<Value>>>,
}

impl Watched {
    async fn open(layout: &Layout) -> Watched {
        let source = Arc::new(HubSource::new(layout.clone()));
        let user = lr_net::current_os_user().unwrap();
        let running = Dashboard::new(source, DashboardOptions { user })
            .unwrap()
            .serve()
            .await
            .expect("the dashboard serves");
        let url = running.url().to_string();
        let (addr, query) = url
            .strip_prefix("http://")
            .unwrap()
            .split_once('/')
            .unwrap();
        let login = http(addr, &format!("/{query}")).await;
        let cookie = login
            .lines()
            .find_map(|l| {
                l.strip_prefix("set-cookie: ")
                    .or_else(|| l.strip_prefix("Set-Cookie: "))
            })
            .and_then(|c| c.split(';').next())
            .expect("the token gives a session cookie")
            .to_string();
        let mut s = TcpStream::connect(addr).await.unwrap();
        let head = format!("GET /api/events HTTP/1.1\r\nHost: {addr}\r\nCookie: {cookie}\r\n\r\n");
        s.write_all(head.as_bytes()).await.unwrap();
        let mut seen = Vec::new();
        let mut buf = [0u8; 4096];
        while !String::from_utf8_lossy(&seen).contains("\r\n\r\n") {
            let n = s.read(&mut buf).await.unwrap();
            assert!(n > 0, "the stream closed before its head");
            seen.extend_from_slice(&buf[..n]);
        }
        let text = String::from_utf8_lossy(&seen).to_string();
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        tokio::spawn(async move {
            let mut pending = text.split_once("\r\n\r\n").unwrap().1.to_string();
            loop {
                while let Some(i) = pending.find('\n') {
                    let line: String = pending.drain(..=i).collect();
                    if let Some(data) = line.trim().strip_prefix("data:") {
                        if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                            sink.lock().unwrap().push(v);
                        }
                    }
                }
                match s.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => pending.push_str(&String::from_utf8_lossy(&buf[..n])),
                }
            }
        });
        Watched { running, events }
    }

    fn about(&self, job: &JobId) -> Vec<Value> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| {
                let ev = &e["event"];
                ev["job"].as_str() == Some(&job.0) || ev["job"]["job"].as_str() == Some(&job.0)
            })
            .cloned()
            .collect()
    }

    async fn until(&self, what: &str, f: impl Fn(&Value) -> bool) -> Value {
        eventually(what, Duration::from_secs(60), || {
            let found = self.events.lock().unwrap().iter().find(|e| f(e)).cloned();
            async move { found }
        })
        .await
    }
}

fn queued_of(job: &JobId) -> impl Fn(&Value) -> bool + '_ {
    move |e| e["event"]["type"] == "job_queued" && e["event"]["job"]["job"] == job.0.as_str()
}

/// A broker with `others_orders = others`, its hub and runner `r1`, and a hub of another
/// machine joined to it; both hubs subscribed through their dashboards' SSE.
async fn two_hubs(
    dir: &TempDir,
    others: &str,
) -> (Layout, Layout, Vec<Box<dyn FnOnce()>>, Watched, Watched) {
    let main = layout(
        dir,
        "main",
        &format!("[visibility]\nothers_orders = \"{others}\"\n"),
    );
    add_runner(&main, "r1");
    add_runner(&main, "r2");
    let guest = layout(dir, "guest", "");
    let broker = start_broker(&main, false).await.expect("broker starts");
    let main_hub = start_hub(&main, false).await.expect("hub starts");
    let runners = start_runners(&main, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    runner_online(&main, "r1").await;
    let (address, code) = hub_code(&main).await;
    assert_eq!(
        hub_join(&guest, &address, &code, Some("main".into())).await,
        Ok(0)
    );
    let guest_hub = start_hub(&guest, false).await.expect("guest hub starts");
    eventually(
        "the guest hub reads the broker",
        Duration::from_secs(30),
        || async {
            let client = hub(&guest).await.ok()?;
            let answer = read(&client, "main", ReadRequest::Runners).await.ok();
            client.close().await;
            answer.map(|_| ())
        },
    )
    .await;
    let owner = Watched::open(&main).await;
    let other = Watched::open(&guest).await;
    let stops: Vec<Box<dyn FnOnce()>> = vec![
        Box::new(move || runners.stop()),
        Box::new(move || guest_hub.stop()),
        Box::new(move || main_hub.stop()),
        Box::new(move || broker.stop()),
    ];
    (main, guest, stops, owner, other)
}

async fn run_quick(main: &Layout, repo: &std::path::Path) -> JobId {
    let (client, job) = submit(main, repo, "quick", None).await;
    let ended = until_exit(client, &job, Duration::from_secs(120)).await;
    assert_eq!(ended.state, JobState::Passed, "{ended:?}");
    job
}

async fn finish(stops: Vec<Box<dyn FnOnce()>>, watched: [Watched; 2], dir: TempDir) {
    for w in watched {
        w.running.shutdown().await;
    }
    for stop in stops {
        stop();
    }
    remove(dir).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_dashboard_streams_broker_events_masked_for_a_client_seeing_only_summaries() {
    let dir = TempDir::new("e2e-events-summary");
    let (main, _guest, stops, owner, other) = two_hubs(&dir, "summary").await;
    let repo = repo(&dir, QUICK);
    let job = run_quick(&main, &repo).await;

    let own = owner.until("the owner's job_queued", queued_of(&job)).await;
    assert_eq!(own["broker"], "local", "{own}");
    assert!(
        own["event"]["job"]["cwd"].is_string(),
        "the owner sees the directory: {own}"
    );
    owner
        .until("the owner's job_finished", |e| {
            e["event"]["type"] == "job_finished" && e["event"]["job"] == job.0.as_str()
        })
        .await;

    let seen = other
        .until("the other client's job_queued", queued_of(&job))
        .await;
    assert_eq!(seen["broker"], "main", "{seen}");
    let summary = &seen["event"]["job"];
    for hidden in ["cwd", "branch", "note", "project"] {
        assert!(summary[hidden].is_null(), "{hidden} is masked: {seen}");
    }
    assert_eq!(summary["state"], "queued", "{seen}");
    finish(stops, [owner, other], dir).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_without_access_to_other_orders_gets_no_event_about_them() {
    let dir = TempDir::new("e2e-events-none");
    let (main, _guest, mut stops, owner, other) = two_hubs(&dir, "none").await;
    let repo = repo(&dir, QUICK);
    let job = run_quick(&main, &repo).await;
    owner.until("the owner's job_queued", queued_of(&job)).await;

    let r2 = start_runners(&main, Some("r2"), 1, None, false)
        .await
        .expect("runner r2 starts");
    stops.insert(0, Box::new(move || r2.stop()));
    let online = |e: &Value| {
        e["event"]["type"] == "runner_online"
            && e["event"]["name"]
                .as_str()
                .is_some_and(|n| n.ends_with("r2"))
    };
    other
        .until(
            "r2 online at the other client, after the job's events",
            online,
        )
        .await;
    assert!(
        other.about(&job).is_empty(),
        "nothing about another client's job: {:?}",
        other.about(&job)
    );
    assert!(!owner.about(&job).is_empty());
    finish(stops, [owner, other], dir).await;
}
