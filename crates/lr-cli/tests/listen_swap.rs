mod common;

use common::loopback::{place, until_state};
use common::{hub_events::until_online, TempDir};
use lr_abstractions::{Dialer, Frame, HubStore, Transport, TransportError};
use lr_cli::build::KEY_FILE;
use lr_cli::commands::jobs::cancel;
use lr_cli::commands::join_cmd::{hub_join, runner_join, RunnerJoin};
use lr_cli::ipc_client::IpcClient;
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_core::JobState;
use lr_net::{ClientKey, TlsDialer};
use lr_proto::{
    AdminReply, AdminRequest, BrokerToHub, CapabilitiesQuery, Hello, HubToBroker, PROTOCOL_VERSION,
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

const LABEL: &str = "swap";
const PLATFORM: &str = "swap-remote";

fn policy(address: &str) -> String {
    policy_with(&format!("\"{address}\""))
}

fn policy_with(value: &str) -> String {
    let user = lr_net::current_os_user().unwrap();
    format!("[listen]\naddress = {value}\n\n[[local_user]]\nname = {user:?}\nlabel = \"{LABEL}\"\n")
}

fn broker_layout(dir: &TempDir) -> Layout {
    let root = dir.path().join("broker");
    let config = dir.path().join("broker-config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("policy.toml"), policy("127.0.0.1:0")).unwrap();
    Layout::new(root, config)
}

fn free_address() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

async fn admin(layout: &Layout, req: AdminRequest) -> AdminReply {
    let client = IpcClient::connect_broker_admin(&layout.admin_ipc())
        .await
        .expect("admin channel");
    let reply = client.call(&req).await.expect("admin reply");
    client.close().await;
    reply
}

async fn code(layout: &Layout, name: &str, runners: Option<u32>) -> (String, String) {
    let req = AdminRequest::BrokerAdd {
        name: Some(name.into()),
        label: None,
        runners,
        address: None,
    };
    match admin(layout, req).await {
        AdminReply::JoinCode { address, code, .. } => (address, code),
        other => panic!("broker add answered {other:?}"),
    }
}

async fn move_listen(layout: &Layout, address: &str) -> AdminReply {
    std::fs::write(layout.config_file("policy.toml"), policy(address)).unwrap();
    admin(layout, AdminRequest::PolicyReload).await
}

async fn close_listen(layout: &Layout) -> AdminReply {
    std::fs::write(layout.config_file("policy.toml"), policy_with("[]")).unwrap();
    admin(layout, AdminRequest::PolicyReload).await
}

struct Member {
    dialer: TlsDialer,
    fingerprint: String,
    name: String,
}

async fn join_hub(dir: &TempDir, layout: &Layout, name: &str) -> (Member, String) {
    let (address, code) = code(layout, name, None).await;
    let hub = Layout::new(dir.path().join(name), dir.path().join(name));
    assert_eq!(hub_join(&hub, &address, &code, None).await, Ok(0));
    let store = lr_store::SqliteHubStore::open(&hub.root.join("hub.db")).unwrap();
    let fingerprint = store.brokers().await.unwrap()[0].fingerprint.clone();
    let key = ClientKey::load(&hub.root.join(KEY_FILE)).unwrap();
    let member = Member {
        dialer: TlsDialer::new(Arc::new(key)),
        fingerprint,
        name: name.into(),
    };
    (member, address)
}

async fn next_message(t: &dyn Transport) -> Result<BrokerToHub, TransportError> {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), t.recv())
            .await
            .expect("the broker answers within 10 s")?;
        let m: BrokerToHub = frame.parse().expect("a broker message");
        if !matches!(m, BrokerToHub::Heartbeat) {
            return Ok(m);
        }
    }
}

async fn connect(member: &Member, address: &str) -> Box<dyn Transport> {
    let live = member
        .dialer
        .dial(address, Some(&member.fingerprint))
        .await
        .unwrap_or_else(|e| panic!("dial {address}: {e}"));
    let hello = HubToBroker::Hello(Hello {
        version: PROTOCOL_VERSION,
        name: member.name.clone(),
        capabilities: None,
    });
    live.send(Frame::json(&hello).unwrap()).await.unwrap();
    let welcome = next_message(&*live).await;
    assert!(
        matches!(welcome, Ok(BrokerToHub::Welcome(_))),
        "{address}: {welcome:?}"
    );
    live
}

async fn round_trip(live: &dyn Transport, req: u64) {
    let query = HubToBroker::CapabilitiesQuery(CapabilitiesQuery { req, probe: None });
    live.send(Frame::json(&query).unwrap()).await.unwrap();
    match next_message(live).await {
        Ok(BrokerToHub::CapabilitiesReply(r)) => assert_eq!(r.req, req),
        other => panic!("the established connection answered {other:?}"),
    }
}

fn reload_ok(reply: &AdminReply) {
    match reply {
        AdminReply::Ok { warnings } => assert!(
            warnings.iter().all(|w| !w.contains("restart")),
            "[listen] applies without a restart: {warnings:?}"
        ),
        other => panic!("policy reload answered {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn listen_swap_accepts_on_new_address_and_keeps_old_connection() {
    let dir = TempDir::new("listen-swap");
    let layout = broker_layout(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let (first, old) = join_hub(&dir, &layout, "first-hub").await;
    let live = connect(&first, &old).await;

    let new = free_address();
    reload_ok(&move_listen(&layout, &new.to_string()).await);
    assert_eq!(broker.network_addresses(), [new]);

    let (second, address) = join_hub(&dir, &layout, "second-hub").await;
    assert_eq!(
        address,
        new.to_string(),
        "broker add prints the new address"
    );
    let fresh = connect(&second, &address).await;
    round_trip(&*fresh, 1).await;
    round_trip(&*live, 2).await;
    assert!(
        first
            .dialer
            .dial(&old, Some(&first.fingerprint))
            .await
            .is_err(),
        "the old address no longer accepts"
    );
    broker.stopped().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn listen_swap_failure_keeps_the_old_listener() {
    let dir = TempDir::new("listen-swap-fail");
    let layout = broker_layout(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let (first, old) = join_hub(&dir, &layout, "first-hub").await;
    let live = connect(&first, &old).await;
    let bound = broker.network_addresses();

    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy = taken.local_addr().unwrap().to_string();
    match move_listen(&layout, &busy).await {
        AdminReply::Failed { message } => {
            assert!(
                message.contains(&busy),
                "the error names the address: {message}"
            )
        }
        other => panic!("a reload whose address is taken answered {other:?}"),
    }
    assert_eq!(broker.network_addresses(), bound, "nothing changed");

    let (second, address) = join_hub(&dir, &layout, "second-hub").await;
    assert_eq!(address, old, "the old address still serves joins");
    round_trip(&*connect(&second, &old).await, 1).await;
    round_trip(&*live, 2).await;
    drop(taken);
    broker.stopped().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn listen_swap_after_an_empty_list_binds_the_new_address() {
    let dir = TempDir::new("listen-swap-empty");
    let layout = broker_layout(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let (first, old) = join_hub(&dir, &layout, "first-hub").await;
    let live = connect(&first, &old).await;

    reload_ok(&close_listen(&layout).await);
    assert!(broker.network_addresses().is_empty());
    round_trip(&*live, 1).await;

    reload_ok(&move_listen(&layout, "127.0.0.1:0").await);
    let bound = broker.network_addresses();
    assert_eq!(
        bound.len(),
        1,
        "an empty list does not stop listening for good"
    );
    let fresh = connect(&first, &bound[0].to_string()).await;
    round_trip(&*fresh, 2).await;
    broker.stopped().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn listen_swap_after_an_empty_list_reports_a_busy_address() {
    let dir = TempDir::new("listen-swap-empty-busy");
    let layout = broker_layout(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let (first, old) = join_hub(&dir, &layout, "first-hub").await;
    let live = connect(&first, &old).await;
    reload_ok(&close_listen(&layout).await);

    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy = taken.local_addr().unwrap().to_string();
    match move_listen(&layout, &busy).await {
        AdminReply::Failed { message } => assert!(
            message.contains(&busy),
            "the error names the address: {message}"
        ),
        other => panic!("a reload whose address is taken answered {other:?}"),
    }
    assert!(broker.network_addresses().is_empty(), "nothing changed");
    round_trip(&*live, 1).await;
    drop(taken);
    broker.stopped().await;
}

fn runner_toml() -> String {
    format!(
        "allow_elevated = true\nallow_same_user_as_broker = true\nmem_mb = 1024\ncpu = 1\n\n{}",
        common::env_path_toml()
    )
}

fn repo(dir: &TempDir) -> std::path::PathBuf {
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".localrunner")).unwrap();
    let slow = if cfg!(windows) {
        "waitfor /t 120 lrswap"
    } else {
        "sleep 120"
    };
    let mut profiles = format!(
        "[profile.slow]\ncommand = \"{slow}\"\ntimeout_s = 300\nmem_mb = 256\nplatforms = [\"{PLATFORM}\"]\n"
    );
    for zone in ["lan", "remote"] {
        profiles.push_str(&format!(
            "\n[profile.slow.zone.{zone}]\ncommand = \"{slow}\"\n"
        ));
    }
    std::fs::write(repo.join(".localrunner").join("profiles.toml"), profiles).unwrap();
    common::init_repo(&repo);
    common::git(&repo, &["add", "."]);
    common::git(&repo, &["commit", "-q", "-m", "profiles"]);
    common::git(dir.path(), &["init", "-q", "--bare", "origin.git"]);
    let origin = dir
        .path()
        .join("origin.git")
        .display()
        .to_string()
        .replace('\\', "/");
    let url = match origin.starts_with('/') {
        true => format!("file://{origin}"),
        false => format!("file:///{origin}"),
    };
    common::git(&repo, &["remote", "add", "origin", &url]);
    common::git(&repo, &["push", "-q", "-u", "origin", "HEAD"]);
    repo
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn listen_swap_does_not_drop_a_running_job() {
    let dir = TempDir::new("listen-swap-job");
    let layout = broker_layout(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub = start_hub(&layout, false).await.expect("hub starts");
    let node = Layout::new(dir.path().join("node"), dir.path().join("node-config"));
    std::fs::create_dir_all(&node.config).unwrap();
    std::fs::write(node.config_file("runner.toml"), runner_toml()).unwrap();
    let (address, code) = code(&layout, "box", Some(1)).await;
    let join = RunnerJoin {
        address,
        code,
        name: Some("box".into()),
        count: 1,
        config: None,
        platforms: vec![PLATFORM.into()],
        tags: Vec::new(),
    };
    assert_eq!(runner_join(&node, join).await, Ok(0));
    let remote = start_runners(&node, Some("box"), 1, None, false)
        .await
        .expect("the joined runner starts");
    until_online(&layout, "box").await;

    let repo = repo(&dir);
    let identity = common::session(LABEL, None, &repo).expect("identity");
    let job = place(&layout, identity).await;
    until_state(&layout, &job, JobState::Running).await;

    reload_ok(&move_listen(&layout, &free_address().to_string()).await);
    let still = until_state(&layout, &job, JobState::Running).await;
    assert!(
        still
            .summary
            .runner
            .as_deref()
            .is_some_and(|r| r.ends_with("box")),
        "{still:?}"
    );
    assert_eq!(
        cancel(&layout, Some(job.0.clone()), LABEL, false, false, None).await,
        Ok(0)
    );
    until_state(&layout, &job, JobState::Cancelled).await;

    remote.stopped().await;
    hub.stopped().await;
    broker.stopped().await;
}
