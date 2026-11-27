mod common;

use common::fake_roles::with_runner;
use common::TempDir;
use lr_cli::commands::upgrade_cmd::{owner_env_commands, restart_env_warnings};
use std::collections::BTreeSet;

fn names() -> BTreeSet<String> {
    ["r1".to_string()].into_iter().collect()
}

fn layout(dir: &TempDir, secret_toml: &str) -> lr_cli::paths::Layout {
    let layout = with_runner(dir);
    std::fs::write(
        layout.runner_dir("r1").join("runner.toml"),
        format!("mem_mb = 1024\n[env]\npath = \"/bin\"\n{secret_toml}"),
    )
    .unwrap();
    layout
}

#[test]
fn a_manually_started_runner_with_an_unset_env_secret_is_warned_about_by_name() {
    let dir = TempDir::new("upgrade-secret-warn");
    let layout = layout(&dir, "[secret.TOKEN]\nfrom = \"env:LR_TOKEN_VAR\"\n");
    let found = restart_env_warnings(&layout, &names(), |_| false, |_| None);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("r1"), "{found:?}");
    assert!(found[0].contains("TOKEN"), "{found:?}");
    assert!(found[0].contains("LR_TOKEN_VAR"), "{found:?}");
}

#[test]
fn a_set_variable_a_unit_and_a_file_source_need_no_warning() {
    let dir = TempDir::new("upgrade-secret-quiet");
    let layout = layout(&dir, "[secret.TOKEN]\nfrom = \"env:LR_TOKEN_VAR\"\n");
    let set = restart_env_warnings(&layout, &names(), |_| false, |_| Some("v".into()));
    assert!(set.is_empty(), "{set:?}");
    let unit = restart_env_warnings(&layout, &names(), |_| true, |_| None);
    assert!(unit.is_empty(), "{unit:?}");
    let file = TempDir::new("upgrade-secret-file");
    let layout = layout_file(&file);
    assert!(restart_env_warnings(&layout, &names(), |_| false, |_| None).is_empty());
}

fn layout_file(dir: &TempDir) -> lr_cli::paths::Layout {
    layout(dir, "[secret.TOKEN]\nfrom = \"file:/abs/token\"\n")
}

#[test]
fn a_role_started_through_systemd_gets_the_owner_of_the_lock_and_the_variable_is_cleared_after() {
    let (set, unset) = owner_env_commands("linux", 4321).unwrap();
    assert_eq!(set.0, "systemctl");
    assert_eq!(
        set.1,
        [
            "--user",
            "set-environment",
            "LOCALRUNNER_UPGRADE_LOCK_PID=4321"
        ]
    );
    let unset = unset.unwrap();
    assert_eq!(unset.0, "systemctl");
    assert_eq!(
        unset.1,
        [
            "--user",
            "unset-environment",
            "LOCALRUNNER_UPGRADE_LOCK_PID"
        ]
    );
}

#[test]
fn a_launchd_agent_gets_the_owner_and_keeps_it_because_launchctl_start_returns_before_the_spawn() {
    let (set, unset) = owner_env_commands("macos", 4321).unwrap();
    assert_eq!(set.0, "launchctl");
    assert_eq!(set.1, ["setenv", "LOCALRUNNER_UPGRADE_LOCK_PID", "4321"]);
    assert!(unset.is_none());
}

#[test]
fn other_systems_have_no_service_manager_environment_to_set() {
    assert!(owner_env_commands("windows", 1).is_none());
}
