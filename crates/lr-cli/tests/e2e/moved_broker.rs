use crate::common::TempDir;
use crate::support::*;
use lr_cli::commands::join_cmd::{runner_join, RunnerJoin};
use lr_cli::host::machine_id;
use lr_cli::ipc_client::IpcClient;
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_proto::{AdminReply, AdminRequest};
use std::path::Path;
use std::time::Duration;

async fn join_code(layout: &Layout) -> (String, String) {
    let admin = IpcClient::connect_broker_admin(&layout.admin_ipc())
        .await
        .expect("admin channel");
    let req = AdminRequest::BrokerAdd {
        name: None,
        label: None,
        runners: Some(1),
        address: None,
    };
    let reply: AdminReply = admin.call(&req).await.expect("admin reply");
    admin.close().await;
    match reply {
        AdminReply::JoinCode { address, code, .. } => (address, code),
        other => panic!("broker add answered {other:?}"),
    }
}

fn copy_tree(from: &Path, to: &Path, skip: &str) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let name = e.file_name();
        if name == skip {
            continue;
        }
        let target = to.join(&name);
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &target, skip);
        } else {
            std::fs::copy(e.path(), &target).unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_broker_moved_with_its_state_keeps_its_joined_runner_without_a_new_join() {
    let dir = TempDir::new("e2e-moved");
    let first = layout(&dir, "first", "");
    let node = layout(&dir, "node", "");
    std::fs::write(node.config_file("runner.toml"), runner_toml()).unwrap();
    let broker = start_broker(&first, false).await.expect("broker starts");
    let hub_role = start_hub(&first, false).await.expect("hub starts");
    let (address, code) = join_code(&first).await;
    let join = RunnerJoin {
        address: address.clone(),
        code,
        name: Some("box".into()),
        count: 1,
        config: None,
        platforms: Vec::new(),
        tags: Vec::new(),
    };
    assert_eq!(runner_join(&node, join).await, Ok(0));
    let pinned = std::fs::read_to_string(node.runner_dir("box").join("broker.toml")).unwrap();
    let remote = start_runners(&node, Some("box"), 1, None, false)
        .await
        .expect("the joined runner starts");
    runner_online(&first, "box").await;
    let old_id = machine_id(&first.root).unwrap();

    hub_role.stop();
    broker.stop();
    eventually(
        "the old broker to free its port",
        Duration::from_secs(20),
        || {
            let free = std::net::TcpListener::bind(&address).is_ok();
            async move { free.then_some(()) }
        },
    )
    .await;

    let moved = layout(&dir, "moved", "");
    copy_tree(&first.root, &moved.root, "machine-id");
    let policy = std::fs::read_to_string(moved.config_file("policy.toml"))
        .unwrap()
        .replace("127.0.0.1:0", &address);
    std::fs::write(moved.config_file("policy.toml"), policy).unwrap();
    let broker = start_broker(&moved, false)
        .await
        .expect("the moved broker starts");
    let hub_role = start_hub(&moved, false).await.expect("hub starts");
    runner_online(&moved, "box").await;
    assert_eq!(
        std::fs::read_to_string(node.runner_dir("box").join("broker.toml")).unwrap(),
        pinned,
        "the runner kept its address and pinned fingerprint: no new join"
    );
    let new_id = machine_id(&moved.root).unwrap();
    assert_ne!(
        new_id, old_id,
        "the moved broker has a machine-id of its own"
    );

    remote.stop();
    hub_role.stop();
    broker.stop();
    remove(dir).await;
}
