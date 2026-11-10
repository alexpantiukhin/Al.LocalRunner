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
    RunnerConfig::parse(
        &read("runner.toml"),
        None,
        "box",
        layout.runner_dir("box"),
        host(),
    )
    .unwrap();
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
