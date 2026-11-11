mod common;

use common::TempDir;
use lr_abstractions::HubStore;
use lr_cli::commands::join_cmd::{hub_join, runner_join, RunnerJoin};
use lr_cli::ipc_client::IpcClient;
use lr_cli::paths::Layout;
use lr_cli::roles::start_broker;
use lr_proto::{AdminReply, AdminRequest};

fn broker_layout(dir: &TempDir) -> Layout {
    let root = dir.path().join("broker");
    let config = dir.path().join("broker-config");
    std::fs::create_dir_all(&config).unwrap();
    let user = lr_net::current_os_user().unwrap();
    let policy =
        format!("[listen]\naddress = \"127.0.0.1:0\"\n\n[[local_user]]\nname = {user:?}\n");
    std::fs::write(config.join("policy.toml"), policy).unwrap();
    Layout::new(root, config)
}

async fn admin(layout: &Layout, req: AdminRequest) -> AdminReply {
    let client = IpcClient::connect_broker_admin(&layout.admin_ipc())
        .await
        .expect("admin channel");
    let reply = client.call(&req).await.expect("admin reply");
    client.close().await;
    reply
}

async fn code(layout: &Layout, name: Option<&str>, runners: Option<u32>) -> (String, String) {
    let req = AdminRequest::BrokerAdd {
        name: name.map(str::to_string),
        label: None,
        runners,
        address: None,
    };
    match admin(layout, req).await {
        AdminReply::JoinCode { address, code, .. } => (address, code),
        other => panic!("broker add answered {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_and_runners_join_with_codes_and_keep_the_pinned_broker() {
    let dir = TempDir::new("join");
    let layout = broker_layout(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");

    let (address, hub_code) = code(&layout, Some("ci-hub"), None).await;
    let hub = Layout::new(dir.path().join("hub"), dir.path().join("hub"));
    assert_eq!(
        hub_join(&hub, &address, &hub_code, Some("main".into())).await,
        Ok(0)
    );
    let store = lr_store::SqliteHubStore::open(&hub.root.join("hub.db")).unwrap();
    let links = store.brokers().await.unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].name, "main");
    assert_eq!(links[0].address, address);
    assert!(!links[0].fingerprint.is_empty());
    assert_eq!(
        admin(&layout, AdminRequest::ClientList).await,
        AdminReply::Names {
            names: vec!["ci-hub".into()]
        }
    );
    assert!(
        hub_join(&hub, &address, &hub_code, None).await.is_err(),
        "a code served twice"
    );

    let (address, runner_code) = code(&layout, None, Some(2)).await;
    let node = Layout::new(dir.path().join("node"), dir.path().join("node"));
    let join = RunnerJoin {
        address: address.clone(),
        code: runner_code,
        name: Some("box".into()),
        count: 2,
        config: None,
        platforms: vec!["linux-x64".into()],
        tags: vec!["gpu".into()],
    };
    assert_eq!(runner_join(&node, join.clone()).await, Ok(0));
    for name in ["box-1", "box-2"] {
        let dir = node.runner_dir(name);
        let broker_file = std::fs::read_to_string(dir.join("broker.toml")).unwrap();
        assert!(broker_file.contains(&address), "{broker_file}");
        assert!(broker_file.contains("fingerprint"), "{broker_file}");
        let own = std::fs::read_to_string(dir.join("runner.toml")).unwrap();
        assert!(own.contains("linux-x64") && own.contains("gpu"), "{own}");
        let parsed: toml::Value = toml::from_str(&own).unwrap();
        assert_eq!(
            parsed["env"]["path"].as_str(),
            std::env::var("PATH").ok().as_deref(),
            "runner join writes [env] path from the shell PATH"
        );
    }
    assert_eq!(
        admin(&layout, AdminRequest::RunnerList).await,
        AdminReply::Names {
            names: vec!["box-1".into(), "box-2".into()]
        }
    );
    let again = runner_join(&node, join).await;
    assert!(again.unwrap_err().contains("already"));
    broker.stop();
}
