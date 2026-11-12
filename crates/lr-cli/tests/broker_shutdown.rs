mod common;

use common::TempDir;
use lr_abstractions::{Peer, PeerAccess};
use lr_cli::admin::AdminApi;
use lr_cli::broker_admin::SERVICE_STAYS;
use lr_cli::commands::admin_cmd::simple;
use lr_cli::paths::Layout;
use lr_cli::roles::start_broker;
use lr_core::Endpoint;
use lr_proto::{AdminReply, AdminRequest};
use std::sync::Arc;
use std::time::Duration;

fn layout(dir: &TempDir) -> Layout {
    let root = dir.path().join("state");
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    let user = lr_net::current_os_user().unwrap();
    let policy = format!("[listen]\naddress = []\n\n[[local_user]]\nname = {user:?}\n");
    std::fs::write(config.join("policy.toml"), policy).unwrap();
    Layout::new(root, config)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_shutdown_stops_a_broker_that_is_not_a_service() {
    let dir = TempDir::new("shutdown");
    let layout = layout(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    assert_eq!(simple(&layout, AdminRequest::Shutdown).await, Ok(0));
    tokio::time::timeout(Duration::from_secs(10), broker.wait())
        .await
        .expect("the broker stops after shutdown");
    assert!(
        simple(&layout, AdminRequest::ClientList).await.is_err(),
        "the admin channel is gone"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_service_managed_broker_answers_shutdown_with_a_warning_and_keeps_running() {
    let dir = TempDir::new("shutdown-service");
    let layout = layout(&dir);
    let mut parts = lr_cli::build::broker(&layout, false)
        .await
        .expect("broker parts");
    Arc::get_mut(&mut parts.admin)
        .expect("the only reference")
        .service = true;
    let peer = Peer {
        endpoint: Endpoint::Ipc {
            os_user: lr_net::current_os_user().unwrap(),
        },
        key: None,
        access: PeerAccess::Local,
    };
    let stop = parts.admin.stop.clone();
    let reply = parts.admin.handle(&peer, AdminRequest::Shutdown).await;
    assert_eq!(
        reply,
        AdminReply::Ok {
            warnings: vec![SERVICE_STAYS.to_string()]
        }
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(300), stop.notified())
            .await
            .is_err(),
        "a service-managed broker is not stopped by shutdown"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logging_reload_through_the_cli_applies_a_valid_file_and_refuses_a_broken_one() {
    let dir = TempDir::new("logging-reload");
    let layout = layout(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let file = layout.config_file("logging.toml");
    assert_eq!(simple(&layout, AdminRequest::LoggingReload).await, Ok(0));
    std::fs::write(&file, "level = = broken\n").unwrap();
    assert_eq!(simple(&layout, AdminRequest::LoggingReload).await, Ok(1));
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    assert_eq!(simple(&layout, AdminRequest::LoggingReload).await, Ok(1));
    std::fs::remove_dir(&file).unwrap();
    assert_eq!(simple(&layout, AdminRequest::LoggingReload).await, Ok(0));
    broker.stop();
}
