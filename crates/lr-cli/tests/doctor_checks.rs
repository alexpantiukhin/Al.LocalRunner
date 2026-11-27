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

const LOGIN_ONLY: &str =
    "starts at login only: it stops at logout and does not start after a reboot until someone logs in";

#[test]
fn doctor_warns_that_a_login_autostart_stops_at_logout() {
    use lr_cli::doctor::{autostart_check, parse_linger, AutostartFound};
    for linger in [None, Some(false)] {
        let found = AutostartFound {
            service: None,
            login: true,
            linger,
            local_channel: false,
        };
        let c = autostart_check("box", &found).unwrap();
        assert_eq!(c.name, "runner box autostart");
        assert_eq!(c.status, Status::Warn, "{c:?}");
        assert!(c.detail.starts_with(LOGIN_ONLY), "{c:?}");
        assert!(
            c.detail.contains("autostart on runner box --service"),
            "{c:?}"
        );
    }
    assert_eq!(parse_linger("Linger=no\n"), Some(false));
    let nothing = AutostartFound {
        service: None,
        login: false,
        linger: Some(true),
        local_channel: false,
    };
    assert_eq!(
        autostart_check("box", &nothing),
        None,
        "no autostart, nothing to say"
    );
}

#[test]
fn doctor_sends_a_local_channel_runner_to_join_before_a_service() {
    use lr_cli::doctor::{autostart_check, AutostartFound};
    let local = AutostartFound {
        service: None,
        login: true,
        linger: None,
        local_channel: true,
    };
    let c = autostart_check("box", &local).unwrap();
    assert_eq!(c.status, Status::Warn, "{c:?}");
    assert!(c.detail.starts_with(LOGIN_ONLY), "{c:?}");
    for part in [
        "broker add --runners 1",
        "runner join <address> --name box",
        "autostart on runner box --service",
    ] {
        assert!(c.detail.contains(part), "{part}: {c:?}");
    }
    assert!(
        c.detail.contains("would be refused"),
        "the plain service advice is not given: {c:?}"
    );
}

#[test]
fn doctor_says_ok_for_a_service_or_a_lingering_user_unit() {
    use lr_cli::doctor::{autostart_check, parse_linger, AutostartFound};
    assert_eq!(parse_linger("Linger=yes\n"), Some(true));
    assert_eq!(parse_linger(""), None);
    assert_eq!(parse_linger("Failed to get user: No such user\n"), None);
    let lingering = AutostartFound {
        service: None,
        login: true,
        linger: parse_linger("Linger=yes\n"),
        local_channel: false,
    };
    let c = autostart_check("box", &lingering).unwrap();
    assert_eq!(c.status, Status::Ok, "{c:?}");
    assert!(c.detail.contains("linger"), "{c:?}");
    for running in [Some(true), None] {
        let service = AutostartFound {
            service: Some(running),
            login: false,
            linger: None,
            local_channel: false,
        };
        let c = autostart_check("box", &service).unwrap();
        assert_eq!(c.status, Status::Ok, "{c:?}");
        assert!(c.detail.contains("OS service"), "{c:?}");
    }
    let stopped = AutostartFound {
        service: Some(Some(false)),
        login: false,
        linger: None,
        local_channel: false,
    };
    let c = autostart_check("box", &stopped).unwrap();
    assert_eq!(c.status, Status::Warn, "{c:?}");
    assert!(c.detail.contains("not running"), "{c:?}");
    let both = AutostartFound {
        service: Some(Some(true)),
        login: true,
        linger: None,
        local_channel: false,
    };
    let c = autostart_check("box", &both).unwrap();
    assert_eq!(c.status, Status::Warn, "{c:?}");
    assert!(
        c.detail.contains("autostart off runner box"),
        "a service and a login entry start the runner twice: {c:?}"
    );
}

#[test]
fn doctor_says_whether_a_linux_service_unit_delegates_its_cgroup() {
    use lr_cli::doctor::delegation_check;
    let unit = "[Service]\nUser=alex\nNoNewPrivileges=true\nDelegate=yes\n\n[Install]\n";
    let c = delegation_check("box", unit);
    assert_eq!(c.name, "runner box memory limit");
    assert_eq!(c.status, Status::Ok, "{c:?}");
    assert!(c.detail.contains("Delegate=yes"), "{c:?}");
    let old = unit.replace("Delegate=yes\n", "");
    let c = delegation_check("box", &old);
    assert_eq!(c.status, Status::Warn, "{c:?}");
    assert!(c.detail.contains("runs as `warn`"), "{c:?}");
    assert!(
        c.detail.contains("autostart off runner box --service")
            && c.detail.contains("autostart on runner box --service"),
        "an installed unit is rewritten by a reinstall: {c:?}"
    );
}

#[test]
fn runners_on_another_version_are_named_per_broker() {
    use lr_cli::doctor::{runner_version_checks, Status};
    use lr_core::Timestamp;
    use lr_proto::{RunnerInfo, WireCapabilities};
    let runner = |name: &str, version: Option<&str>| RunnerInfo {
        zone: None,
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
            accept_min_free_disk_mb: None,
            cache_max_mb: None,
            cache_used_mb: None,
            service_mem_mb: Default::default(),
            commands: Default::default(),
            enforcement: None,
            version: version.map(str::to_string),
            enforce_limit: false,
            programs_missing: Default::default(),
        },
        last_seen: Some(Timestamp(1)),
        free_mem_mb: 0,
        free_cpu: 0,
        held_tags: Vec::new(),
        services: Vec::new(),
        unmet: Vec::new(),
        liveness: None,
        silent_for_s: None,
        kind: None,
        pool: None,
        can_start_now: None,
        busy: None,
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

#[test]
fn doctor_warns_about_runners_of_a_broker_that_miss_programs_but_not_about_its_own() {
    use lr_cli::doctor::{runner_program_checks, Status};
    use lr_core::Timestamp;
    use lr_proto::{RunnerInfo, WireCapabilities};
    let runner = |name: &str, missing: &[&str]| RunnerInfo {
        zone: None,
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
            accept_min_free_disk_mb: None,
            cache_max_mb: None,
            cache_used_mb: None,
            service_mem_mb: Default::default(),
            commands: Default::default(),
            enforcement: None,
            version: None,
            enforce_limit: false,
            programs_missing: missing.iter().map(|m| m.to_string()).collect(),
        },
        last_seen: Some(Timestamp(1)),
        free_mem_mb: 0,
        free_cpu: 0,
        held_tags: Vec::new(),
        services: Vec::new(),
        unmet: Vec::new(),
        liveness: None,
        silent_for_s: None,
        kind: None,
        pool: None,
        can_start_now: None,
        busy: None,
    };
    let per = vec![(
        "office".to_string(),
        vec![
            runner("far", &["cargo", "npm"]),
            runner("ok", &[]),
            runner("local:mine", &["git"]),
        ],
    )];
    let checks = runner_program_checks(&per, &["mine".to_string()]);
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert_eq!(checks[0].name, "broker office runner far");
    assert_eq!(checks[0].status, Status::Warn);
    assert_eq!(checks[0].detail, "programs not found: cargo, npm");
}

#[test]
fn the_files_of_an_old_user_broker_are_listed_for_removal() {
    use lr_cli::doctor::stale_broker_files;
    let dir = TempDir::new("stale-files");
    assert!(stale_broker_files(dir.path()).is_empty());
    for name in [
        "broker.db",
        "broker.db-wal",
        "broker.db-shm",
        "broker-cert.pem",
        "broker-key.pem",
        "hub.db",
    ] {
        std::fs::write(dir.path().join(name), "x").unwrap();
    }
    let names: Vec<String> = stale_broker_files(dir.path())
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "broker.db",
            "broker.db-wal",
            "broker.db-shm",
            "broker-cert.pem",
            "broker-key.pem"
        ]
    );
}

#[test]
fn a_second_broker_of_the_user_is_refused_while_the_service_is_installed() {
    use lr_cli::commands::roles_cmd::second_broker_refusal;
    assert!(second_broker_refusal(false, false).is_none());
    assert!(second_broker_refusal(true, true).is_none());
    assert!(second_broker_refusal(false, true)
        .unwrap()
        .contains("--state-dir"));
}

#[test]
fn doctor_warns_about_a_secret_whose_source_gives_nothing_without_its_value() {
    use lr_cli::doctor::{secret_checks, Status};
    use lr_cli::runner_config::SecretFrom;
    let mut sources = std::collections::BTreeMap::new();
    sources.insert(
        "TOKEN".to_string(),
        SecretFrom::Env("LR_SURELY_UNSET_VARIABLE".into()),
    );
    sources.insert(
        "FILE".to_string(),
        SecretFrom::File(std::path::PathBuf::from("/definitely/not/here")),
    );
    let found = secret_checks("r1", &sources);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().all(|c| c.status == Status::Warn));
    let text: String = found
        .iter()
        .map(|c| c.detail.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("secret TOKEN: the variable LR_SURELY_UNSET_VARIABLE is not set"),
        "{text}"
    );
    assert!(
        text.contains("secret FILE: the file /definitely/not/here does not exist"),
        "{text}"
    );
    assert!(text.contains("localrunner secret set r1 TOKEN"), "{text}");
    assert!(secret_checks("r1", &Default::default()).is_empty());
}

#[test]
fn a_refused_connection_says_nothing_listens_and_how_a_service_is_started() {
    use lr_cli::doctor::dial_failure_text;
    for raw in [
        "Connection refused (os error 111)",
        "No connection could be made (os error 10061)",
    ] {
        let text = dial_failure_text("vps.example:7443", raw);
        assert!(
            text.starts_with("vps.example:7443: nothing listens there"),
            "{text}"
        );
        assert!(
            text.contains("systemctl start al-localrunner-broker"),
            "{text}"
        );
        assert!(text.contains("Start-Service AlLocalRunnerBroker"), "{text}");
        assert!(!text.contains("os error"), "{text}");
    }
    assert_eq!(
        dial_failure_text("h:1", "certificate mismatch"),
        "h:1: certificate mismatch"
    );
}

#[test]
fn a_secret_the_runner_unit_sets_is_not_reported_even_when_this_shell_lacks_it() {
    use lr_cli::doctor::secret_checks_in;
    use lr_cli::runner_config::SecretFrom;
    let mut sources = std::collections::BTreeMap::new();
    sources.insert(
        "TOKEN".to_string(),
        SecretFrom::Env("LR_SURELY_UNSET_VARIABLE".into()),
    );
    sources.insert(
        "OTHER".to_string(),
        SecretFrom::Env("LR_SURELY_UNSET_OTHER".into()),
    );
    let mut unit = std::collections::BTreeMap::new();
    unit.insert("LR_SURELY_UNSET_VARIABLE".to_string(), "value".to_string());
    unit.insert("LR_SURELY_UNSET_OTHER".to_string(), String::new());
    let found = secret_checks_in("r1", &sources, Some(&unit));
    assert_eq!(found.len(), 1, "{found:?}");
    let text = &found[0].detail;
    assert!(text.contains("secret OTHER"), "{text}");
    assert!(text.contains("not set by the unit of the runner"), "{text}");
    assert!(!text.contains("runner process"), "{text}");
    assert!(!text.contains("value"), "{text}");
}

#[test]
fn without_a_unit_the_text_names_this_shell_not_the_runner_process() {
    use lr_cli::doctor::secret_checks;
    use lr_cli::runner_config::SecretFrom;
    let mut sources = std::collections::BTreeMap::new();
    sources.insert(
        "TOKEN".to_string(),
        SecretFrom::Env("LR_SURELY_UNSET_VARIABLE".into()),
    );
    let text = secret_checks("r1", &sources)[0].detail.clone();
    assert!(text.contains("not set in this shell"), "{text}");
    assert!(!text.contains("runner process"), "{text}");
}
