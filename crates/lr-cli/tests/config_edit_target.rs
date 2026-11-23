use lr_cli::commands::admin_cmd::read_broker_policy;
use lr_cli::config_edit::{resolve_with, validate, Target};
use lr_cli::paths::{default_root, service_root, Layout};
use std::cell::RefCell;
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lr-cfg-target-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn custom(name: &str) -> (Layout, PathBuf) {
    let dir = scratch(name);
    (Layout::new(dir.clone(), dir.clone()), dir)
}

#[test]
fn targets_parse() {
    assert_eq!("broker".parse::<Target>().unwrap(), Target::Broker);
    assert_eq!("hub".parse::<Target>().unwrap(), Target::Hub);
    assert_eq!("logging".parse::<Target>().unwrap(), Target::Logging);
    assert_eq!(
        "runner:ci".parse::<Target>().unwrap(),
        Target::Runner("ci".into())
    );
    assert!("runner:".parse::<Target>().is_err());
    assert!("nope".parse::<Target>().is_err());
}

#[test]
fn broker_resolves_to_the_service_file_when_the_service_is_installed() {
    let layout = Layout::new(default_root().unwrap(), default_root().unwrap());
    let with = resolve_with(&layout, &Target::Broker, true).unwrap();
    let without = resolve_with(&layout, &Target::Broker, false).unwrap();
    assert_eq!(with, service_root().join("policy.toml"));
    assert_eq!(without, layout.config_file("policy.toml"));
}

#[test]
fn custom_state_dir_ignores_the_installed_service() {
    let (layout, _) = custom("custom");
    let path = resolve_with(&layout, &Target::Broker, true).unwrap();
    assert_eq!(path, layout.config_file("policy.toml"));
}

#[test]
fn runner_prefers_own_copy() {
    let (layout, _) = custom("runner");
    let template = resolve_with(&layout, &Target::Runner("a".into()), false).unwrap();
    assert_eq!(template, layout.config_file("runner.toml"));
    std::fs::create_dir_all(layout.runner_dir("a")).unwrap();
    std::fs::write(layout.runner_dir("a").join("runner.toml"), "").unwrap();
    let own = resolve_with(&layout, &Target::Runner("a".into()), false).unwrap();
    assert_eq!(own, layout.runner_dir("a").join("runner.toml"));
}

#[test]
fn policy_check_without_file_uses_resolved_path() {
    let layout = Layout::new(default_root().unwrap(), default_root().unwrap());
    let asked = RefCell::new(Vec::new());
    let (path, text) = read_broker_policy(&layout, true, |p| {
        asked.borrow_mut().push(p.to_path_buf());
        Ok("# service".into())
    })
    .unwrap();
    assert_eq!(path, service_root().join("policy.toml"));
    assert_eq!(text, "# service");
    assert_eq!(*asked.borrow(), vec![service_root().join("policy.toml")]);
    assert_ne!(path, layout.config_file("policy.toml"));
}

#[test]
fn policy_check_without_service_reads_the_config_copy() {
    let (layout, _) = custom("nosvc");
    let (path, _) = read_broker_policy(&layout, false, |_| Ok(String::new())).unwrap();
    assert_eq!(path, layout.config_file("policy.toml"));
}

#[test]
fn policy_check_missing_file_is_an_empty_policy() {
    let (layout, _) = custom("missing");
    let (_, text) =
        read_broker_policy(&layout, false, |_| Err(std::io::ErrorKind::NotFound.into())).unwrap();
    assert_eq!(text, "");
}

#[test]
fn policy_check_access_denied_names_the_path_and_the_rights() {
    let layout = Layout::new(default_root().unwrap(), default_root().unwrap());
    let err = read_broker_policy(&layout, true, |_| {
        Err(std::io::ErrorKind::PermissionDenied.into())
    })
    .unwrap_err();
    assert!(err.contains(&service_root().join("policy.toml").display().to_string()));
    assert!(err.contains("administrator rights"));
    assert!(err.contains("sudo"));
}

#[test]
fn hub_and_logging_resolve_to_the_config_dir() {
    let (layout, _) = custom("files");
    assert_eq!(
        resolve_with(&layout, &Target::Hub, false).unwrap(),
        layout.config_file("hub-policy.toml")
    );
    assert_eq!(
        resolve_with(&layout, &Target::Logging, false).unwrap(),
        layout.config_file("logging.toml")
    );
}

#[test]
fn validate_policy_rejects_unknown_key() {
    let (layout, _) = custom("policy");
    assert!(validate(&Target::Broker, "", &layout).is_ok());
    assert!(validate(&Target::Broker, "no_such_key = 1\n", &layout).is_err());
}

#[test]
fn validate_hub_and_logging_reject_garbage() {
    let (layout, _) = custom("hubl");
    assert!(validate(&Target::Hub, "no_such_key = 1\n", &layout).is_err());
    assert!(validate(&Target::Logging, "no_such_key = 1\n", &layout).is_err());
}

const RUNNER: &str = "[env]\npath = \"/usr/bin\"\n";

#[test]
fn validate_runner_refuses_a_new_unknown_key() {
    let (layout, dir) = custom("newkey");
    std::fs::write(dir.join("runner.toml"), RUNNER).unwrap();
    let target = Target::Runner("a".into());
    let edited = format!("{RUNNER}misspelled = 1\n");
    assert!(validate(&target, &edited, &layout).is_err());
    assert!(validate(&target, RUNNER, &layout).unwrap().is_empty());
}

#[test]
fn validate_runner_keeps_old_unknown_key_warning() {
    let (layout, dir) = custom("oldkey");
    let old = format!("{RUNNER}misspelled = 1\n");
    std::fs::write(dir.join("runner.toml"), &old).unwrap();
    let target = Target::Runner("a".into());
    let edited = format!("mem_mb = 512\n{old}");
    let warnings = validate(&target, &edited, &layout).unwrap();
    assert_eq!(warnings.len(), 1);
}

#[test]
fn validate_runner_refuses_swapping_one_unknown_key_for_another() {
    let (layout, dir) = custom("swap");
    std::fs::write(dir.join("runner.toml"), format!("{RUNNER}misspelled = 1\n")).unwrap();
    let target = Target::Runner("a".into());
    let edited = format!("{RUNNER}another_typo = 1\n");
    assert!(validate(&target, &edited, &layout).is_err());
}
