mod common;

use common::TempDir;
use lr_cli::doctor::{checks_with, Check, Status};
use lr_cli::paths::Layout;
use lr_net::{BrokerIdentity, ClientKey, ListenAddr, Registry, TlsListener};
use lr_store::SqliteBrokerStore;
use std::process::Command;
use std::sync::Arc;

fn find<'a>(all: &'a [Check], name: &str) -> &'a Check {
    all.iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no check {name}: {all:?}"))
}

fn runner_with_broker(layout: &Layout, name: &str, address: &str, fingerprint: &str) {
    let dir = layout.runner_dir(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("runner.toml"), "mem_mb = 1024\ncpu = 1\n").unwrap();
    std::fs::write(
        dir.join("broker.toml"),
        format!("address = {address:?}\nfingerprint = {fingerprint:?}\n"),
    )
    .unwrap();
    ClientKey::generate()
        .unwrap()
        .save(&dir.join("participant.key"))
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_names_elevation_busy_ports_and_a_broker_with_another_certificate() {
    let dir = TempDir::new("doctor-checks");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    std::fs::create_dir_all(&layout.config).unwrap();

    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = busy.local_addr().unwrap().port();
    std::fs::write(
        layout.config_file("policy.toml"),
        format!("[listen]\naddress = [\"127.0.0.1:{port}\"]\n"),
    )
    .unwrap();

    let identity = BrokerIdentity::generate().unwrap();
    let store = Arc::new(SqliteBrokerStore::open(&dir.path().join("broker.db")).unwrap());
    let registry = Arc::new(Registry::new(
        store,
        lr_cli::build::clock(),
        "m-broker",
        identity.fingerprint(),
    ));
    let listener = TlsListener::bind(
        ListenAddr::parse("127.0.0.1:0", false).unwrap(),
        &identity,
        registry,
    )
    .await
    .unwrap();
    let impostor = BrokerIdentity::generate()
        .unwrap()
        .fingerprint()
        .to_string();
    runner_with_broker(&layout, "r1", &listener.local_addr().to_string(), &impostor);
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed_addr = closed.local_addr().unwrap().to_string();
    drop(closed);
    runner_with_broker(
        &layout,
        "r2",
        &closed_addr,
        &identity.fingerprint().to_string(),
    );

    let ordinary = checks_with(&layout, false).await;
    assert_eq!(find(&ordinary, "privileges").status, Status::Ok);
    let elevated = checks_with(&layout, true).await;
    let rights = find(&elevated, "privileges");
    assert_eq!(rights.status, Status::Warn);
    assert!(rights.detail.contains("allow_elevated"), "{rights:?}");

    let port_check = find(&ordinary, &format!("port 127.0.0.1:{port}"));
    assert_eq!(port_check.status, Status::Warn, "{port_check:?}");

    let pinned = find(&ordinary, "runner r1 broker");
    assert_eq!(pinned.status, Status::Fail);
    assert!(pinned.detail.contains("wrong_fingerprint"), "{pinned:?}");
    let gone = find(&ordinary, "runner r2 broker");
    assert_eq!(gone.status, Status::Fail, "{gone:?}");

    let out = Command::new(env!("CARGO_BIN_EXE_localrunner"))
        .args(["doctor", "--json"])
        .env("LOCALRUNNER_STATE_DIR", &layout.root)
        .env("LOCALRUNNER_CONFIG_DIR", &layout.config)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a failed check gives exit code 1"
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let statuses: Vec<&str> = json
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["status"].as_str().unwrap())
        .collect();
    assert!(
        statuses.contains(&"fail") && statuses.contains(&"warn"),
        "{json}"
    );
    drop(busy);
}

fn broker_and_runner(name: &str, policy: &str) -> (TempDir, Layout) {
    let dir = TempDir::new(name);
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    std::fs::create_dir_all(&layout.config).unwrap();
    std::fs::write(layout.config_file("policy.toml"), policy).unwrap();
    let runner = layout.runner_dir("r1");
    std::fs::create_dir_all(&runner).unwrap();
    std::fs::write(runner.join("runner.toml"), "mem_mb = 1024\ncpu = 1\n").unwrap();
    (dir, layout)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_warns_about_a_runner_under_the_brokers_user_that_runs_any_command() {
    let (_dir, layout) = broker_and_runner("doctor-same-user", "");
    let all = checks_with(&layout, false).await;
    let risk = find(&all, "broker and runner user");
    assert_eq!(risk.status, Status::Warn);
    assert!(risk.detail.contains("spec 3.12"), "{risk:?}");
    assert!(lr_cli::doctor::same_user_risk(&layout, true).is_some());
    assert_eq!(
        lr_cli::doctor::same_user_risk(&layout, false),
        None,
        "up without a runner in the plan says nothing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn named_commands_for_every_client_need_no_warning() {
    let (_dir, layout) = broker_and_runner("doctor-named", "[commands]\ndefault = \"named\"\n");
    let all = checks_with(&layout, false).await;
    assert!(
        all.iter().all(|c| c.name != "broker and runner user"),
        "{all:?}"
    );
}
