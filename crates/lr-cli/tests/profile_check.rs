use lr_cli::commands::profile_cmd::{report, Machine};
use lr_cli::runner_config::SecretFrom;
use std::collections::BTreeMap;

const PROFILES: &str = r#"
[profile.build]
command = "cargo build"
timeout_s = 60
secrets = ["registry_cred"]

[profile.plain]
command = "true"
timeout_s = 5
"#;

const POLICY: &str = r#"
[[local_user]]
name = "alex"
label = "dev"

[[secrets.allow]]
label = "dev"
names = ["registry_cred"]
"#;

fn machine(label: &str, source: Option<SecretFrom>, set: fn(&str) -> bool) -> Machine {
    let mut sources = BTreeMap::new();
    if let Some(s) = source {
        sources.insert("registry_cred".to_string(), s);
    }
    Machine {
        policy: Some(lr_broker::policy::load(POLICY).unwrap()),
        label: label.into(),
        runners: vec![("r1".into(), sources)],
        env_set: set,
    }
}

fn profiles() -> Vec<lr_core::Profile> {
    lr_core::parse_profiles(PROFILES).unwrap()
}

#[test]
fn a_secret_that_is_allowed_has_a_source_and_a_set_variable_passes() {
    let m = machine("dev", Some(SecretFrom::Env("PKG_CRED".into())), |_| true);
    let (lines, ok) = report(&profiles(), &m);
    let text = lines.join("\n");
    assert!(ok, "{text}");
    assert!(text.contains("as the variable REGISTRY_CRED"), "{text}");
    assert!(text.contains("allowed for the label \"dev\""), "{text}");
    assert!(text.contains("env:PKG_CRED, set in this shell"), "{text}");
    assert!(!text.contains("profile plain"), "{text}");
}

#[test]
fn every_problem_is_named_in_one_run_with_the_secret_and_the_fix() {
    let m = machine("agent", Some(SecretFrom::Env("PKG_CRED".into())), |_| false);
    let (lines, ok) = report(&profiles(), &m);
    let text = lines.join("\n");
    assert!(!ok);
    assert!(
        text.contains("NOT allowed for the label \"agent\""),
        "{text}"
    );
    assert!(text.contains("env:PKG_CRED is not set"), "{text}");
    let none = machine("dev", None, |_| true);
    let text = report(&profiles(), &none).0.join("\n");
    assert!(text.contains("no [secret.registry_cred]"), "{text}");
}

#[test]
fn an_unreadable_policy_is_said_not_guessed() {
    let mut m = machine("dev", Some(SecretFrom::Env("PKG_CRED".into())), |_| true);
    m.policy = None;
    let (lines, ok) = report(&profiles(), &m);
    assert!(ok);
    assert!(lines.join("\n").contains("policy: not checked"));
}
