use crate::common::TempDir;
use crate::dashboard_events::{queued_of, Watched};
use crate::kit::*;
use crate::support::*;
use lr_cli::commands::jobs::hub;
use lr_cli::commands::log_cmd::{find_log, LogQuery};
use lr_core::JobId;
use lr_obs::Redactor;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn snapshot(w: &Watched) -> Value {
    let mut s = TcpStream::connect(&w.addr)
        .await
        .expect("dashboard listens");
    let head = format!(
        "GET /api/snapshot HTTP/1.1\r\nHost: {}\r\nCookie: {}\r\nConnection: close\r\n\r\n",
        w.addr, w.cookie
    );
    s.write_all(head.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut out)).await;
    let text = String::from_utf8_lossy(&out).into_owned();
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    let body = text.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    let start = body.find('{').unwrap_or(0);
    let end = body.rfind('}').map_or(body.len(), |e| e + 1);
    serde_json::from_str(&body[start..end]).unwrap_or_else(|e| panic!("{e}: {body}"))
}

fn job_of<'a>(snap: &'a Value, job: &JobId) -> Option<&'a Value> {
    snap["jobs"]
        .as_array()?
        .iter()
        .find(|j| j["detail"]["summary"]["job"] == job.0.as_str())
}

// scenario: 16.23
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dashboard_streams_queue_updates_after_the_token_exchange() {
    let dir = TempDir::new("e2e-dash");
    let gate = dir.path().join("gate");
    let mut release = Release::new(&[&gate]);
    let (main, mut roles) = machine(&dir, "main", "", &["r1"], &runner_toml()).await;
    let profiles = format!(
        "{}\n{}",
        held_profile("busy", dir.path(), &gate, 120, "mem_mb = 256\n"),
        echo_profile("next", "after-the-busy-one", 60, "mem_mb = 256\n"),
    );
    let repo = repo(&dir, &profiles);
    let (busy_client, busy) = submit(&main, &repo, "busy", None).await;
    release.watch(started(dir.path(), "busy", "child").await);

    let view = status(&main).await.expect("status");
    assert!(
        view.running.iter().any(|r| r.summary.job == busy),
        "{view:?}"
    );
    eventually(
        "the log of the running job to hold what its child printed",
        Duration::from_secs(30),
        || async {
            let client = hub(&main).await.expect("hub channel");
            let query = LogQuery {
                job: busy.clone(),
                step: None,
                tail: None,
                follow: false,
            };
            let mut log = String::new();
            find_log(
                &client,
                None,
                &query,
                &Arc::new(Redactor::new(&[])),
                &mut |t| log.push_str(t),
            )
            .await
            .expect("the log of the running job");
            client.close().await;
            log.contains("child-started").then_some(())
        },
    )
    .await;

    let page = Watched::open(&main).await;
    let (next_client, next) = submit(&main, &repo, "next", None).await;
    let event = page.until("job_queued over SSE", queued_of(&next)).await;
    assert_eq!(event["broker"], "local", "{event}");
    let snap = snapshot(&page).await;
    let running = job_of(&snap, &busy).unwrap_or_else(|| panic!("{snap}"));
    assert_eq!(running["list"], "running", "{snap}");
    assert_eq!(running["stale"], false, "{snap}");
    let waiting = job_of(&snap, &next).unwrap_or_else(|| panic!("{snap}"));
    assert_eq!(waiting["list"], "queued", "{snap}");

    roles.take_broker().stop();
    let stale = eventually(
        "the last snapshot of the stopped broker, marked stale",
        Duration::from_secs(60),
        || async {
            let snap = snapshot(&page).await;
            let down = snap["brokers"]
                .as_array()?
                .iter()
                .any(|b| b["name"] == "local" && b["available"] == false);
            let kept = job_of(&snap, &busy)?.clone();
            (down && kept["stale"] == true).then_some(kept)
        },
    )
    .await;
    assert_eq!(stale["list"], "running", "{stale}");

    page.running.shutdown().await;
    busy_client.close().await;
    next_client.close().await;
    roles.stop();
    drop(release);
    remove(dir).await;
}
