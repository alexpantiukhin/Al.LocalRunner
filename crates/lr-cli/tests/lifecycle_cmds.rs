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
