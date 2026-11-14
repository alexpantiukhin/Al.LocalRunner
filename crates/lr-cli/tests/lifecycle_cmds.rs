mod common;

use async_trait::async_trait;
use common::TempDir;
use lr_abstractions::{Listener, Peer};
use lr_cli::admin::{serve, AdminApi};
use lr_cli::commands::down_cmd::{drained, stop_broker};
use lr_cli::commands::log_cmd::cli_redactor;
use lr_cli::commands::snapshot_cmd::save;
use lr_cli::commands::up_cmd::startable;
use lr_cli::init::NodeRole;
use lr_cli::paths::Layout;
use lr_net::{IpcAccess, IpcListener};
use lr_proto::{AdminReply, AdminRequest};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

fn layout(dir: &TempDir) -> Layout {
    let root = dir.path().join("state");
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    Layout::new(root, config)
}

struct ExitsBeforeAnswering {
    asked: Arc<Notify>,
}

#[async_trait]
impl AdminApi for ExitsBeforeAnswering {
    async fn handle(&self, _peer: &Peer, req: AdminRequest) -> AdminReply {
        if matches!(req, AdminRequest::Shutdown) {
            self.asked.notify_one();
            panic!("the broker process ends before its answer leaves");
        }
        AdminReply::Ok {
            warnings: Vec::new(),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broker_that_stops_before_answering_counts_as_stopped() {
    let dir = TempDir::new("lost-answer");
    let layout = layout(&dir);
    layout.prepare_ipc().unwrap();
    let listener = IpcListener::bind(&layout.admin_ipc(), IpcAccess::OwnerAndAdmins)
        .await
        .expect("admin channel");
    let listener: Arc<dyn Listener> = Arc::new(listener);
    let asked = Arc::new(Notify::new());
    let api = Arc::new(ExitsBeforeAnswering {
        asked: asked.clone(),
    });
    let server = tokio::spawn(serve(listener, api));
    let exit = tokio::spawn(async move {
        asked.notified().await;
        server.abort();
    });
    let stopped = tokio::time::timeout(Duration::from_secs(20), stop_broker(&layout))
        .await
        .expect("down ends");
    assert_eq!(stopped, Ok(Some(Vec::new())));
    exit.await.unwrap();
}

#[tokio::test]
async fn waiting_for_draining_runners_ends_when_they_go_or_on_interrupt() {
    assert!(drained(|| async { false }, std::future::pending()).await);
    assert!(!drained(|| async { true }, async {}).await);
}

#[test]
fn up_leaves_a_broker_service_to_the_service_manager() {
    let all = vec![NodeRole::Hub, NodeRole::Broker, NodeRole::Runner];
    assert_eq!(startable(all.clone(), false), all);
    assert_eq!(startable(all, true), vec![NodeRole::Hub, NodeRole::Runner]);
    assert!(startable(vec![NodeRole::Broker], true).is_empty());
}

#[test]
fn cli_output_masks_the_redact_fields_of_logging_toml() {
    let dir = TempDir::new("redact-fields");
    let layout = layout(&dir);
    let line = "deploy build_pin=pin4242pin done\n";
    assert!(cli_redactor(&layout)
        .redact_text(line)
        .contains("pin4242pin"));
    std::fs::write(
        layout.config_file("logging.toml"),
        "[redact]\nfields = [\"build_pin\"]\n",
    )
    .unwrap();
    let masked = cli_redactor(&layout).redact_text(line);
    assert!(!masked.contains("pin4242pin"), "{masked}");
    assert!(masked.contains("deploy"), "{masked}");
}

#[test]
fn a_snapshot_file_is_readable_by_its_owner_only() {
    let dir = TempDir::new("snapshot-file");
    let path = dir.path().join("temp").join("runner").join("queue.html");
    save(&path, "<html>log</html>").expect("saved");
    let bytes = lr_net::read_private(&path).expect("only the owner can read it");
    assert_eq!(bytes, b"<html>log</html>");
}

#[test]
fn status_masks_the_last_lines_of_running_jobs() {
    use lr_cli::commands::view::{mask_status, status_json};
    use lr_core::{JobId, JobState, Timestamp};
    use lr_obs::Redactor;
    use lr_proto::{JobSummary, PoolLoad, RunningView, StatusView};
    let summary = JobSummary {
        job: JobId("j1".into()),
        client_label: "dev".into(),
        state: JobState::Running,
        platform: None,
        runner: None,
        queued_at: Timestamp(1),
        started_at: None,
        finished_at: None,
        exit_code: None,
        reason: None,
        project: None,
        label: None,
        profile: None,
        source: None,
        note: None,
        branch: None,
        cwd: None,
        timeout_s: None,
        github_run_url: None,
        peak_mem_mb: None,
        peak_disk_mb: None,
        labels: Vec::new(),
    };
    let view = StatusView {
        running: vec![RunningView {
            summary,
            steps: Vec::new(),
            last_lines: vec!["login api_token=hunter2hunter2".into(), "ok".into()],
        }],
        queue: Vec::new(),
        recent: Vec::new(),
        pool: PoolLoad {
            used_mem_mb: 0,
            total_mem_mb: 1024,
            used_cpu: 0,
            total_cpu: 1,
        },
        next: None,
    };
    let mut views = vec![("local".to_string(), view)];
    mask_status(&mut views, &Redactor::new(&[]));
    let json = status_json(&views).to_string();
    assert!(!json.contains("hunter2hunter2"), "{json}");
    assert!(json.contains("login"), "{json}");
}

#[test]
fn the_dashboard_log_tail_is_masked_whole_lines_only() {
    use lr_cli::dashboard::masked_tail;
    use lr_dashboard::LOG_TAIL_BYTES;
    use lr_obs::Redactor;
    let r = Redactor::new(&[]);
    let short = masked_tail(b"start\napi_token=hunter2hunter2\n", &r);
    let short = String::from_utf8(short).unwrap();
    assert!(short.starts_with("start\n"), "{short}");
    assert!(!short.contains("hunter2hunter2"), "{short}");
    let mut long = "api_token=".to_string();
    long.push_str(&"s".repeat(LOG_TAIL_BYTES));
    long.push_str("\nlast line\n");
    let tail = String::from_utf8(masked_tail(long.as_bytes(), &r)).unwrap();
    assert_eq!(
        tail, "last line\n",
        "a line cut by the tail start is left out"
    );
}

#[test]
fn uninstall_stops_short_when_a_role_did_not_stop() {
    use lr_cli::commands::setup::uninstall_may_go_on;
    assert_eq!(uninstall_may_go_on(Ok(0)), Ok(()));
    let refused = uninstall_may_go_on(Ok(1)).unwrap_err();
    assert!(refused.contains("nothing was removed"), "{refused}");
    let failed = uninstall_may_go_on(Err("no principal".into())).unwrap_err();
    assert!(failed.contains("no principal"), "{failed}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_listener_stops_answering_while_still_held() {
    use lr_cli::commands::up_cmd::broker_answers;
    let dir = TempDir::new("closed-listener");
    let layout = layout(&dir);
    layout.prepare_ipc().unwrap();
    let held = Arc::new(
        IpcListener::bind(&layout.broker_ipc(), IpcAccess::OwnerOnly)
            .await
            .expect("broker IPC"),
    );
    assert!(broker_answers(&layout).await);
    held.close();
    let mut gone = false;
    for _ in 0..50 {
        if !broker_answers(&layout).await {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(gone, "a closed listener answers while something holds it");
    drop(held);
}

#[cfg(unix)]
#[tokio::test]
async fn a_socket_file_left_behind_is_not_a_running_broker() {
    use lr_cli::commands::up_cmd::{broker_answers, running_roles};
    use std::os::unix::net::UnixListener;
    let dir = TempDir::new("stale-socket");
    let layout = layout(&dir);
    layout.prepare_ipc().unwrap();
    for path in [layout.broker_ipc(), layout.admin_ipc()] {
        drop(UnixListener::bind(&path).expect("bind"));
        assert!(std::path::Path::new(&path).exists(), "{path} stays behind");
    }
    assert!(!broker_answers(&layout).await);
    assert!(!running_roles(&layout).await.broker);
    std::fs::remove_file(layout.broker_ipc()).unwrap();
    let live = UnixListener::bind(layout.broker_ipc()).expect("bind");
    assert!(broker_answers(&layout).await, "a listening socket answers");
    drop(live);
    assert!(!broker_answers(&layout).await);
}
