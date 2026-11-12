mod common;

use common::TempDir;
use lr_cli::init::{self, NodeRole};
use lr_cli::paths::Layout;
use lr_cli::runner_config::RunnerConfig;
use std::path::Path;
use std::process::{Command, Output};

fn layout(dir: &TempDir) -> Layout {
    Layout::new(dir.path().join("state"), dir.path().join("config"))
}

fn localrunner(dir: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_localrunner"))
        .args(args)
        .env("LOCALRUNNER_STATE_DIR", dir.path().join("state"))
        .env("LOCALRUNNER_CONFIG_DIR", dir.path().join("config"))
        .output()
        .expect("the binary runs")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn host() -> lr_broker::policy::HostResources {
    lr_cli::host::resources()
}

#[test]
fn init_writes_templates_every_loader_accepts() {
    let dir = TempDir::new("templates");
    let layout = layout(&dir);
    let created = init::init(&layout, &NodeRole::ALL, "alex").unwrap();
    assert!(created.iter().all(|c| c.written), "{created:?}");
    let read = |name: &str| std::fs::read_to_string(layout.config_file(name)).unwrap();
    let policy = lr_broker::policy::load(&read("policy.toml")).unwrap();
    assert_eq!(policy.local_users[0].name, "alex");
    lr_hub::parse_hub_policy(&read("hub-policy.toml")).unwrap();
    lr_obs::LoggingConfig::parse(&read("logging.toml")).unwrap();
    let runner = RunnerConfig::parse(
        &read("runner.toml"),
        None,
        "box",
        layout.runner_dir("box"),
        host(),
    )
    .expect("the runner.toml of init starts a runner");
    assert_eq!(runner.settings.path, std::env::var("PATH").ok());
    let without_path = init::runner_template(None);
    assert!(
        RunnerConfig::parse(&without_path, None, "box", layout.runner_dir("box"), host()).is_err()
    );
    assert!(layout.root.join("machine-id").exists());
    assert_eq!(init::roles(&layout), NodeRole::ALL.to_vec());
}

#[test]
fn init_again_keeps_edited_files_and_adds_roles() {
    let dir = TempDir::new("again");
    let layout = layout(&dir);
    init::init(&layout, &[NodeRole::Runner], "alex").unwrap();
    std::fs::write(layout.config_file("runner.toml"), "tags = [\"mine\"]\n").unwrap();
    let again = init::init(&layout, &[NodeRole::Runner, NodeRole::Hub], "alex").unwrap();
    let runner = again
        .iter()
        .find(|c| c.path.ends_with("runner.toml"))
        .unwrap();
    assert!(!runner.written);
    assert_eq!(
        std::fs::read_to_string(layout.config_file("runner.toml")).unwrap(),
        "tags = [\"mine\"]\n"
    );
    assert_eq!(init::roles(&layout), vec![NodeRole::Hub, NodeRole::Runner]);
}

fn exists(p: &Path) -> bool {
    p.exists()
}

#[test]
fn install_dry_run_prints_the_plan_and_changes_nothing() {
    let dir = TempDir::new("install");
    let bin = dir.path().join("bin");
    let out = localrunner(
        &dir,
        &[
            "install",
            "--dir",
            bin.to_str().unwrap(),
            "--allow-elevated",
            "--dry-run",
        ],
    );
    let all = text(&out);
    assert!(out.status.success(), "{all}");
    assert!(all.contains(bin.to_str().unwrap()), "{all}");
    assert!(all.to_lowercase().contains("path"), "{all}");
    assert!(!exists(&bin), "dry run created {}", bin.display());
    assert!(!exists(&dir.path().join("state")));
}

#[test]
fn autostart_and_service_dry_runs_change_nothing() {
    let dir = TempDir::new("autostart");
    for args in [
        &["autostart", "on", "runner", "--dry-run"][..],
        &["autostart", "off", "hub", "--dry-run"][..],
        &["broker", "install-service", "--dry-run"][..],
        &["uninstall", "--purge", "--dry-run", "--dir", "nowhere"][..],
    ] {
        let out = localrunner(&dir, args);
        let all = text(&out);
        assert!(!all.trim().is_empty(), "{args:?} printed no plan");
        assert!(
            out.status.code() == Some(0) || args[0] == "uninstall",
            "{args:?}: {all}"
        );
    }
    let out = localrunner(&dir, &["broker", "install-service", "--dry-run"]);
    let all = text(&out);
    let account = lr_cli::os_service::service_account(lr_cli::os_service::Os::current());
    assert!(all.contains(account), "{all}");
    assert!(!exists(&dir.path().join("state").join("roles")));
}

#[test]
fn autostart_without_a_role_or_init_is_refused() {
    let dir = TempDir::new("no-role");
    let out = localrunner(&dir, &["autostart", "on", "--dry-run"]);
    assert!(!out.status.success());
    assert!(text(&out).contains("init"), "{}", text(&out));
}

#[test]
fn doctor_json_reports_a_broken_runner_config_and_fails() {
    let dir = TempDir::new("doctor");
    let layout = layout(&dir);
    let runner = layout.runner_dir("box");
    std::fs::create_dir_all(&runner).unwrap();
    std::fs::write(runner.join("runner.toml"), "platforms = 7\n").unwrap();
    let out = localrunner(&dir, &["doctor", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let checks: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let checks = checks.as_array().unwrap();
    let find = |name: &str| {
        checks
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("no {name} in {checks:?}"))
    };
    assert_eq!(find("runner box")["status"], "fail");
    assert_eq!(find("state directory")["status"], "ok");
    assert!(find("process tree")["status"] != "fail");
}

#[test]
fn autostart_of_one_named_runner_and_uninstall_take_off_only_their_entries() {
    let dir = TempDir::new("one-runner");
    let out = localrunner(&dir, &["autostart", "on", "runner", "box", "--dry-run"]);
    let all = text(&out);
    assert!(out.status.success(), "{all}");
    assert!(all.contains("runner-box"), "{all}");
    if cfg!(windows) {
        assert!(all.contains("schtasks") && all.contains("/Create"), "{all}");
        assert!(!all.contains(r"CurrentVersion\Run"), "{all}");
    }
    let off = localrunner(&dir, &["autostart", "off", "runner", "box", "--dry-run"]);
    assert!(text(&off).contains("runner-box"), "{}", text(&off));
    let hub = localrunner(&dir, &["autostart", "on", "hub", "box", "--dry-run"]);
    assert!(!hub.status.success());
    assert!(text(&hub).contains("runner"), "{}", text(&hub));
    let bad = localrunner(&dir, &["autostart", "on", "runner", "CON", "--dry-run"]);
    assert!(!bad.status.success());

    let bin = dir.path().join("bin");
    let out = localrunner(
        &dir,
        &["uninstall", "--dry-run", "--dir", bin.to_str().unwrap()],
    );
    let all = text(&out);
    assert!(all.contains("from PATH"), "{all}");
    if cfg!(windows) {
        assert!(all.contains("localrunnerw.exe"), "{all}");
    }
}

#[cfg(unix)]
#[test]
fn init_closes_the_state_directory_to_its_owner() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("mode");
    let layout = layout(&dir);
    std::fs::create_dir_all(&layout.root).unwrap();
    std::fs::set_permissions(&layout.root, std::fs::Permissions::from_mode(0o755)).unwrap();
    init::init(&layout, &[NodeRole::Hub], "alex").unwrap();
    let mode = std::fs::metadata(&layout.root)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[test]
fn an_existing_state_directory_does_not_stop_a_role_from_starting() {
    let dir = TempDir::new("existing");
    let root = dir.path().join("state");
    std::fs::create_dir_all(&root).unwrap();
    init::ensure_state_dir(&root).expect("an existing directory is used as it is");
    let fresh = dir.path().join("fresh").join("state");
    init::ensure_state_dir(&fresh).expect("a missing directory is created");
    assert!(fresh.is_dir());
}

#[cfg(unix)]
#[test]
fn an_open_state_directory_of_this_user_is_closed() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let dir = TempDir::new("mode-existing");
    let shared = dir.path().join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o770)).unwrap();
    init::ensure_state_dir(&shared).unwrap();
    assert_eq!(mode(&shared), 0o700);
    let created = dir.path().join("created");
    init::ensure_state_dir(&created).unwrap();
    assert_eq!(mode(&created), 0o700);
}

#[cfg(unix)]
#[test]
fn a_state_directory_of_another_user_is_closed_by_root_and_named_once() {
    use std::os::unix::fs::PermissionsExt;
    if lr_net::current_os_principal().ok().as_deref() != Some("0") {
        return;
    }
    let dir = TempDir::new("mode-foreign");
    let mounted = dir.path().join("mounted");
    std::fs::create_dir_all(&mounted).unwrap();
    std::os::unix::fs::chown(&mounted, Some(12345), Some(12345)).unwrap();
    std::fs::set_permissions(&mounted, std::fs::Permissions::from_mode(0o755)).unwrap();
    init::ensure_state_dir(&mounted).expect("a bind mount of another uid starts with a warning");
    init::ensure_state_dir(&mounted).unwrap();
    let mode = std::fs::metadata(&mounted).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    let warnings: Vec<String> = lr_cli::state_dir::take_warnings()
        .into_iter()
        .filter(|w| w.contains("mounted"))
        .collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("uid 12345"), "{}", warnings[0]);
}

#[cfg(windows)]
#[test]
fn an_open_state_directory_of_this_user_is_closed() {
    let dir = TempDir::new("acl-existing");
    let root = dir.path().join("state");
    std::fs::create_dir_all(&root).unwrap();
    init::ensure_state_dir(&root).unwrap();
    let access = lr_cli::state_dir::access(&root).unwrap();
    assert!(access.own && !access.open, "{access:?}");
    std::fs::write(root.join("f"), "x").unwrap();
    assert_eq!(std::fs::read_to_string(root.join("f")).unwrap(), "x");
}

#[test]
fn a_foreign_open_state_directory_refuses_and_others_warn() {
    use lr_cli::state_dir::{verdict, DirAccess, Verdict};
    let dir = Path::new("/srv/state");
    let at = |own: bool, open: bool| DirAccess {
        owner: Some("uid 1000".into()),
        own,
        open,
        detail: "mode 755".into(),
    };
    assert_eq!(verdict(dir, &at(true, false), None), Verdict::Use);
    match verdict(dir, &at(false, true), Some("Operation not permitted")) {
        Verdict::Refuse(t) => {
            for part in [
                "uid 1000",
                "mode 755",
                "Operation not permitted",
                "localrunner init",
                "chown",
            ] {
                assert!(t.contains(part), "{part}: {t}");
            }
        }
        other => panic!("{other:?}"),
    }
    match verdict(dir, &at(false, false), None) {
        Verdict::Warn(t) => assert!(t.contains("uid 1000") && t.contains("mode 755"), "{t}"),
        other => panic!("{other:?}"),
    }
    match verdict(dir, &at(true, true), Some("Read-only file system")) {
        Verdict::Warn(t) => assert!(t.contains("Read-only file system"), "{t}"),
        other => panic!("{other:?}"),
    }
}
