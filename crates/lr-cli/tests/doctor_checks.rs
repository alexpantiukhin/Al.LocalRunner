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

const OTHER_USER: &str = "[[local_user]]\nname = \"bob\"\n";
const RISK: &str = "runner r1 and broker user";

fn broker_and_runner(name: &str, policy: &str) -> (TempDir, Layout) {
    broker_and_runner_with(name, policy, "")
}

fn broker_and_runner_with(name: &str, policy: &str, runner_extra: &str) -> (TempDir, Layout) {
    let dir = TempDir::new(name);
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    std::fs::create_dir_all(&layout.config).unwrap();
    std::fs::write(layout.config_file("policy.toml"), policy).unwrap();
    let runner = layout.runner_dir("r1");
    std::fs::create_dir_all(&runner).unwrap();
    std::fs::write(
        runner.join("runner.toml"),
        format!(
            "{runner_extra}allow_elevated = true\nmem_mb = 1024\ncpu = 1\n{}",
            common::env_path_toml()
        ),
    )
    .unwrap();
    (dir, layout)
}

const ALLOW: &str = "allow_same_user_as_broker = true\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_fails_a_runner_under_the_brokers_user_that_runs_any_command() {
    let (_dir, layout) = broker_and_runner("doctor-same-user", OTHER_USER);
    let all = checks_with(&layout, false).await;
    let risk = find(&all, RISK);
    assert_eq!(risk.status, Status::Fail, "{risk:?}");
    assert!(risk.detail.contains("spec 3.12"), "{risk:?}");
    assert!(
        risk.detail.contains("allow_same_user_as_broker = true"),
        "{risk:?}"
    );
    assert!(lr_cli::doctor::same_user_risk(&layout, true).is_some());
    assert_eq!(
        lr_cli::doctor::same_user_risk(&layout, false),
        None,
        "up without a runner in the plan says nothing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_only_warns_when_runner_toml_accepts_the_same_user_risk() {
    let (_dir, layout) = broker_and_runner_with("doctor-same-user-allowed", OTHER_USER, ALLOW);
    let all = checks_with(&layout, false).await;
    let risk = find(&all, RISK);
    assert_eq!(risk.status, Status::Warn, "{risk:?}");
    assert!(risk.detail.contains("spec 3.12"), "{risk:?}");
}

#[test]
fn the_verdict_refuses_without_the_setting_warns_with_it_and_ignores_a_lone_owner() {
    let (_dir, layout) = broker_and_runner("verdict-same-user", OTHER_USER);
    let refusal = lr_cli::doctor::same_user_verdict(&layout, false).unwrap_err();
    assert!(
        refusal.contains("refuses") && refusal.contains("allow_same_user_as_broker"),
        "{refusal}"
    );
    let warning = lr_cli::doctor::same_user_verdict(&layout, true).unwrap();
    assert!(warning.is_some_and(|w| w.contains("spec 3.12")));
    assert!(!lr_cli::doctor::runner_allows_same_user(&layout, "r1"));

    let user = lr_net::current_os_user().unwrap();
    let owner_only = format!("[[local_user]]\nname = {user:?}\n");
    let (_dir, single) = broker_and_runner("verdict-owner-only", &owner_only);
    assert_eq!(
        lr_cli::doctor::same_user_verdict(&single, false),
        Ok(None),
        "the [[local_user]] of the owner, as init writes it, admits nobody else"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn named_commands_for_every_client_need_no_warning() {
    let policy = format!("{OTHER_USER}\n[commands]\ndefault = \"named\"\n");
    let (_dir, layout) = broker_and_runner("doctor-named", &policy);
    let all = checks_with(&layout, false).await;
    assert!(all.iter().all(|c| c.name != RISK), "{all:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broker_only_its_owner_can_reach_needs_no_warning_and_no_policy_means_the_default() {
    let (_dir, layout) = broker_and_runner("doctor-owner-only", "");
    assert_eq!(lr_cli::doctor::same_user_risk(&layout, true), None);
    let all = checks_with(&layout, false).await;
    assert!(all.iter().all(|c| c.name != RISK), "{all:?}");
    std::fs::write(
        layout.config_file("policy.toml"),
        "[listen]\naddress = \"127.0.0.1:0\"\n",
    )
    .unwrap();
    assert!(lr_cli::doctor::same_user_risk(&layout, true).is_some());
    std::fs::remove_file(layout.config_file("policy.toml")).unwrap();
    assert_eq!(
        lr_cli::doctor::same_user_risk(&layout, true),
        None,
        "the default policy listens nowhere and names no other user"
    );
}

async fn up_runner(layout: &Layout) -> (i32, Vec<String>) {
    let started = std::sync::Mutex::new(Vec::new());
    let spawn = |argv: &[String]| -> std::io::Result<u32> {
        started.lock().unwrap().push(argv[..2].join(" "));
        Err(std::io::Error::other("not started in the test"))
    };
    let code = lr_cli::commands::up_cmd::up_with(
        layout,
        Some(lr_cli::commands::RoleArg::Runner),
        std::time::Duration::from_secs(1),
        &spawn,
    )
    .await
    .unwrap();
    (code, started.into_inner().unwrap())
}

#[tokio::test(start_paused = true)]
async fn up_refuses_a_runner_under_the_brokers_user_unless_runner_toml_accepts_it() {
    let (_dir, refused) = broker_and_runner("up-same-user", OTHER_USER);
    let (code, started) = up_runner(&refused).await;
    assert_eq!(code, 1);
    assert!(started.is_empty(), "{started:?}");

    let (_dir, allowed) = broker_and_runner_with("up-same-user-allowed", OTHER_USER, ALLOW);
    let (_, started) = up_runner(&allowed).await;
    assert_eq!(started, vec!["runner run"]);

    let user = lr_net::current_os_user().unwrap();
    let owner_only = format!("[[local_user]]\nname = {user:?}\n");
    let (_dir, single) = broker_and_runner("up-owner-only", &owner_only);
    let (_, started) = up_runner(&single).await;
    assert_eq!(
        started,
        vec!["runner run"],
        "the single local setup is untouched"
    );
}

fn runner_run(layout: &Layout) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_localrunner"))
        .args(["runner", "run", "--name", "r1"])
        .env("LOCALRUNNER_STATE_DIR", &layout.root)
        .env("LOCALRUNNER_CONFIG_DIR", &layout.config)
        .output()
        .unwrap()
}

#[test]
fn runner_run_refuses_to_start_under_the_brokers_user_without_the_setting() {
    let (_dir, layout) = broker_and_runner("run-same-user", OTHER_USER);
    let out = runner_run(&layout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("refuses to start") && stderr.contains("allow_same_user_as_broker"),
        "{stderr}"
    );
}

#[test]
fn doctor_reports_a_broker_database_that_no_service_state_uses() {
    use lr_cli::doctor::stale_broker_db;
    let dir = TempDir::new("doctor-stale-db");
    assert!(
        stale_broker_db(dir.path(), true, false).is_none(),
        "no file"
    );
    std::fs::write(dir.path().join("broker.db"), b"").unwrap();
    let found = stale_broker_db(dir.path(), true, false).expect("left over");
    assert_eq!(found.status, Status::Warn);
    assert!(found.detail.contains("left over"), "{}", found.detail);
    assert!(
        stale_broker_db(dir.path(), false, false).is_none(),
        "a broker started by hand owns the file"
    );
    assert!(
        stale_broker_db(dir.path(), true, true).is_none(),
        "the service's own state"
    );
}

#[test]
fn runners_on_another_version_are_named_per_broker() {
    use lr_cli::doctor::{runner_version_checks, Status};
    use lr_core::Timestamp;
    use lr_proto::{RunnerInfo, WireCapabilities};
    let runner = |name: &str, version: Option<&str>| RunnerInfo {
        name: name.into(),
        online: true,
        caps: WireCapabilities {
            platforms: Default::default(),
            mem_mb: 1,
            cpu: 1,
            disk_mb: 1,
            programs: Default::default(),
            tags: Default::default(),
            max_job_timeout_s: None,
            services: Default::default(),
            machine: None,
            accept_min_mem_mb: None,
            cache_max_mb: None,
            cache_used_mb: None,
            service_mem_mb: Default::default(),
            commands: Default::default(),
            enforcement: None,
            version: version.map(str::to_string),
        },
        last_seen: Some(Timestamp(1)),
        free_mem_mb: 0,
        free_cpu: 0,
        held_tags: Vec::new(),
        services: Vec::new(),
        unmet: Vec::new(),
    };
    let per = vec![
        ("a".to_string(), vec![runner("r1", Some("1.2.0"))]),
        (
            "b".to_string(),
            vec![runner("r2", Some("1.1.0")), runner("r3", None)],
        ),
    ];
    let found = runner_version_checks(&per, "1.2.0");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].status, Status::Warn);
    assert!(
        found[0].detail.contains("r2 (1.1.0), r3 (unknown)"),
        "{}",
        found[0].detail
    );
    assert!(runner_version_checks(&per[..1], "1.2.0").is_empty());
}
