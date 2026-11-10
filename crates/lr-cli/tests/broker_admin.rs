mod common;

use common::TempDir;
use lr_cli::ipc_client::IpcClient;
use lr_cli::paths::Layout;
use lr_cli::roles::start_broker;
use lr_proto::{AdminReply, AdminRequest, IpcChannel};

fn layout(dir: &TempDir, listen: &str) -> Layout {
    let root = dir.path().join("state");
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    let user = lr_net::current_os_user().unwrap();
    let policy = format!("[listen]\naddress = {listen}\n\n[[local_user]]\nname = {user:?}\n");
    std::fs::write(config.join("policy.toml"), policy).unwrap();
    Layout::new(root, config)
}

async fn admin(layout: &Layout, req: AdminRequest) -> AdminReply {
    let client = IpcClient::connect(&layout.admin_ipc(), IpcChannel::Admin)
        .await
        .expect("admin channel");
    let reply = client.call(&req).await.expect("admin reply");
    client.close().await;
    reply
}

fn add(name: &str) -> AdminRequest {
    AdminRequest::BrokerAdd {
        name: Some(name.into()),
        label: None,
        runners: None,
        address: None,
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_broker_add_starts_one_listener() {
    let dir = TempDir::new("adds");
    let layout = layout(&dir, "\"127.0.0.1:0\"");
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let (a, b) = tokio::join!(admin(&layout, add("one")), admin(&layout, add("two")));
    let address = |r: AdminReply| match r {
        AdminReply::JoinCode { address, .. } => address,
        other => panic!("broker add answered {other:?}"),
    };
    assert_eq!(address(a), address(b));
    assert_eq!(broker.network_addresses().len(), 1);
    broker.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_listen_keeps_no_listener_and_can_be_retried() {
    let dir = TempDir::new("partial");
    let port = free_port();
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = blocker.local_addr().unwrap().port();
    let listen = format!("[\"127.0.0.1:{port}\", \"127.0.0.1:{taken}\"]");
    let layout = layout(&dir, &listen);
    let broker = start_broker(&layout, false).await.expect("broker starts");

    let first = admin(&layout, add("one")).await;
    assert!(matches!(first, AdminReply::Failed { .. }), "{first:?}");
    assert!(broker.network_addresses().is_empty());
    let free = std::net::TcpListener::bind(("127.0.0.1", port));
    assert!(
        free.is_ok(),
        "the first address stayed bound after the failure"
    );
    drop(free);

    drop(blocker);
    let second = admin(&layout, add("one")).await;
    assert!(matches!(second, AdminReply::JoinCode { .. }), "{second:?}");
    assert_eq!(broker.network_addresses().len(), 2);
    broker.stop();
}
