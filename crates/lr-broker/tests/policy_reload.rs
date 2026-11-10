mod common;

use common::HOST;
use lr_broker::policy::{ActivePolicy, RestartOnly};
use lr_core::Strategy;

const START: &str = r#"
[listen]
address = "192.168.1.10:7443"

[networks]
home = ["192.168.1.0/24"]

[grace]
lost_min = 3
"#;

fn started() -> ActivePolicy {
    ActivePolicy::start(START, HOST).expect("start policy").0
}

#[test]
fn start_refuses_an_invalid_file() {
    let e = ActivePolicy::start("[grace]\nlost_min = \"x\"\n", HOST).unwrap_err();
    assert_eq!(e.key.as_deref(), Some("lost_min"));
}

#[test]
fn start_returns_warnings() {
    let text = "[[rule]]\npriority = 1\n[[rule]]\npriority = 2\n";
    let (_, warnings) = ActivePolicy::start(text, HOST).unwrap();
    assert!(
        warnings.iter().any(|w| w.message.contains("never")),
        "{warnings:?}"
    );
}

#[test]
fn reload_applies_rules_and_queue() {
    let mut active = started();
    let text = format!("{START}\n[queue]\nstrategy = \"weighted\"\n\n[[rule]]\npriority = 9\n");
    let applied = active.reload(&text).unwrap();
    assert!(applied.restart_required.is_empty());
    assert_eq!(active.file().queue.strategy, Strategy::Weighted);
    assert_eq!(active.file().rules[0].rule.priority, Some(9));
    assert_eq!(active.policy().pool().cpu, HOST.cpu);
}

#[test]
fn networks_change_on_reload_reported_not_applied() {
    let mut active = started();
    let text = START.replace("192.168.1.0/24", "10.0.0.0/24");
    let applied = active.reload(&text).unwrap();
    assert_eq!(applied.restart_required, [RestartOnly::Networks]);
    let home: Vec<String> = active.file().networks["home"]
        .iter()
        .map(|n| n.to_string())
        .collect();
    assert_eq!(home, ["192.168.1.0/24"]);
}

#[test]
fn pending_restart_reported_until_reverted() {
    let mut active = started();
    let moved = START.replace("192.168.1.10", "192.168.1.11");
    assert_eq!(
        active.reload(&moved).unwrap().restart_required,
        [RestartOnly::Listen]
    );
    let moved_again = format!("{moved}\n[queue]\nage_after_min = 5\n");
    assert_eq!(
        active.reload(&moved_again).unwrap().restart_required,
        [RestartOnly::Listen]
    );
    assert_eq!(active.file().queue.age_after_min, 5);
    assert!(active.reload(START).unwrap().restart_required.is_empty());
}

#[test]
fn changed_grace_is_flagged_for_runners() {
    let mut active = started();
    let text = START.replace("lost_min = 3", "lost_min = 5");
    let applied = active.reload(&text).unwrap();
    let update = applied.settings_update.expect("grace changed");
    assert_eq!(update.broker_lost_kill_s, Some(300));
    assert_eq!(active.file().grace.lost_min.get(), 5);
    let again = active.reload(&text).unwrap();
    assert_eq!(again.settings_update, None);
}

#[test]
fn unchanged_grace_is_not_flagged() {
    let mut active = started();
    let text = format!("{START}\n[queue]\nage_after_min = 10\n");
    assert_eq!(active.reload(&text).unwrap().settings_update, None);
}

#[test]
fn reload_with_warnings_still_applies() {
    let mut active = started();
    let text = format!("{START}\n[[rule]]\npriority = 1\n\n[[rule]]\npriority = 2\n");
    let applied = active.reload(&text).unwrap();
    assert!(!applied.warnings.is_empty());
    assert_eq!(active.file().rules.len(), 2);
}

#[test]
fn invalid_reload_keeps_listen_networks_and_grace() {
    let mut active = started();
    let broken = START.replace("lost_min = 3", "lost_min = 0");
    assert!(active.reload(&broken).is_err());
    assert_eq!(active.file().grace.lost_min.get(), 3);
    assert_eq!(active.file().listen.addresses.len(), 1);
    assert!(active.file().networks.contains_key("home"));
}
