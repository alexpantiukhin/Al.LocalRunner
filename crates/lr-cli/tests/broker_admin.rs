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

fn reloads(broker: &lr_cli::roles::Running, result: &str) -> u64 {
    broker.metrics()[0]
        .snapshot()
        .into_iter()
        .filter(|p| p.name == "localrunner_policy_reload_total")
        .filter(|p| p.labels == [("result".to_string(), result.to_string())])
        .map(|p| match p.value {
            lr_obs::metrics::Value::Counter(n) => n,
            other => panic!("{other:?}"),
        })
        .sum()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_broker_reports_its_metrics_into_the_logger_registry() {
    let dir = TempDir::new("metrics");
    let layout = layout(&dir, "[]");
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let ok = admin(&layout, AdminRequest::PolicyReload).await;
    assert!(matches!(ok, AdminReply::Ok { .. }), "{ok:?}");
    let policy = layout.config_file("policy.toml");
    std::fs::write(&policy, "[grace]\nlost_min = 0\n").unwrap();
    let invalid = admin(&layout, AdminRequest::PolicyReload).await;
    assert!(matches!(invalid, AdminReply::Failed { .. }), "{invalid:?}");
    std::fs::remove_file(&policy).unwrap();
    std::fs::create_dir(&policy).unwrap();
    let unreadable = admin(&layout, AdminRequest::PolicyReload).await;
    assert!(
        matches!(unreadable, AdminReply::Failed { .. }),
        "{unreadable:?}"
    );
    assert_eq!(reloads(&broker, "ok"), 1);
    assert_eq!(reloads(&broker, "error"), 2);
    let names: Vec<&str> = broker.metrics()[0]
        .snapshot()
        .iter()
        .map(|p| p.name)
        .collect();
    assert!(
        names.contains(&"localrunner_saturation_wait_seconds"),
        "{names:?}"
    );
    assert!(names.contains(&"localrunner_outbox_pending"), "{names:?}");
    assert_eq!(broker.metrics()[0].rejected(), 0);
    broker.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broker_add_prints_the_address_given_or_the_first_listen_address() {
    let dir = TempDir::new("add-address");
    let held = [
        std::net::TcpListener::bind("127.0.0.1:0").unwrap(),
        std::net::TcpListener::bind("127.0.0.1:0").unwrap(),
    ];
    let [first, second] = held.map(|l| l.local_addr().unwrap().port());
    let listen = format!("[\"127.0.0.1:{first}\", \"127.0.0.1:{second}\"]");
    let layout = layout(&dir, &listen);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let address = |r: AdminReply| match r {
        AdminReply::JoinCode { address, .. } => address,
        other => panic!("broker add answered {other:?}"),
    };
    assert_eq!(
        address(admin(&layout, add("one")).await),
        format!("127.0.0.1:{first}"),
        "the first [listen] address"
    );
    let given = AdminRequest::BrokerAdd {
        name: Some("two".into()),
        label: None,
        runners: None,
        address: Some("pool.example.org:7443".into()),
    };
    assert_eq!(
        address(admin(&layout, given).await),
        "pool.example.org:7443"
    );
    broker.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn policy_check_names_the_file_the_running_broker_uses() {
    let dir = TempDir::new("policy-path");
    let layout = layout(&dir, "[]");
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let reply = admin(&layout, AdminRequest::PolicyCheck).await;
    let AdminReply::Policy { path, rules, .. } = reply else {
        panic!("{reply:?}");
    };
    assert_eq!(
        std::path::PathBuf::from(path),
        layout.config_file("policy.toml")
    );
    let asked = lr_cli::commands::admin_cmd::broker_policy(&layout).await;
    assert!(rules[0].starts_with("rule 1 ci-first"), "{rules:?}");
    assert!(matches!(asked, Some(Ok(_))), "{asked:?}");
    assert_eq!(
        lr_cli::commands::admin_cmd::policy_check(&layout, None).await,
        Ok(0)
    );
    std::fs::write(layout.config_file("policy.toml"), "[grace]\nlost_min = 0\n").unwrap();
    assert_eq!(
        lr_cli::commands::admin_cmd::policy_check(&layout, None).await,
        Ok(1),
        "the file the broker reads is broken, whatever a copy elsewhere says"
    );
    broker.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn policy_check_without_a_broker_reads_the_local_file() {
    let dir = TempDir::new("policy-local");
    let layout = layout(&dir, "[]");
    assert!(lr_cli::commands::admin_cmd::broker_policy(&layout)
        .await
        .is_none());
    assert_eq!(
        lr_cli::commands::admin_cmd::policy_check(&layout, None).await,
        Ok(0)
    );
}
