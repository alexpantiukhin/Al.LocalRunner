mod common;

use common::HOST;
use lr_broker::policy::{ActivePolicy, ListenValue};
use lr_core::Strategy;
use std::net::{IpAddr, Ipv4Addr};

const START: &str = r#"
[listen]
address = "192.168.1.10:7443"

[networks]
home = ["192.168.1.0/24"]

[grace]
lost_min = 3
"#;

fn started() -> ActivePolicy {
    ActivePolicy::start(START, HOST, false)
        .expect("start policy")
        .0
}

#[test]
fn start_refuses_an_invalid_file() {
    let e = ActivePolicy::start("[grace]\nlost_min = \"x\"\n", HOST, false).unwrap_err();
    assert_eq!(e.key.as_deref(), Some("lost_min"));
}

#[test]
fn start_returns_warnings() {
    let text = "[[rule]]\npriority = 1\n[[rule]]\npriority = 2\n";
    let (_, warnings) = ActivePolicy::start(text, HOST, false).unwrap();
    assert!(
        warnings.iter().any(|w| w.message.contains("never")),
        "{warnings:?}"
    );
}

#[test]
fn reload_applies_rules_and_queue() {
    let mut active = started();
    let text = format!("{START}\n[queue]\nstrategy = \"weighted\"\n\n[[rule]]\npriority = 9\n");
    active.reload(&text).unwrap();
    assert_eq!(active.file().queue.strategy, Strategy::Weighted);
    assert_eq!(active.file().rules[0].rule.priority, Some(9));
    assert_eq!(active.policy().pool().cpu, HOST.cpu);
}

#[test]
fn reload_applies_networks_and_shared() {
    let mut active = started();
    let text = format!(
        "{}
[[shared]]
members = [\"a\", \"b\"]
",
        START.replace("192.168.1.0/24", "10.0.0.0/24")
    );
    active.reload(&text).unwrap();
    let home: Vec<String> = active.file().networks["home"]
        .iter()
        .map(|n| n.to_string())
        .collect();
    assert_eq!(home, ["10.0.0.0/24"]);
    assert_eq!(active.file().shared.len(), 1);
    assert_eq!(active.file().shared[0].members, ["a", "b"]);
    active.reload(START).unwrap();
    assert!(active.file().shared.is_empty());
}

#[test]
fn reload_applies_listen() {
    let mut active = started();
    let moved = START.replace("192.168.1.10", "192.168.1.11");
    active.reload(&moved).unwrap();
    assert_eq!(
        active.file().listen.addresses,
        [ListenValue::Explicit(
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 11)),
            Some(7443)
        )]
    );
}

#[test]
fn reload_refuses_container_listen_outside_a_container() {
    let mut active = started();
    let e = active
        .reload("[listen]\naddress = \"container\"\n")
        .unwrap_err();
    assert_eq!(e.table.as_deref(), Some("listen"));
    assert_eq!(active.file().listen.addresses.len(), 1);
    let (mut inside, _) = ActivePolicy::start(START, HOST, true).unwrap();
    inside
        .reload("[listen]\naddress = \"container\"\n")
        .unwrap();
    assert_eq!(inside.file().listen.addresses, [ListenValue::Container]);
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
    let text = format!("{START}\n[queue]\nstrategy = \"fair-share\"\n");
    assert_eq!(active.reload(&text).unwrap().settings_update, None);
}

#[test]
fn reload_with_age_after_min_applies_and_warns() {
    let mut active = started();
    let text = format!("{START}\n[queue]\nage_after_min = 10\n");
    let applied = active.reload(&text).unwrap();
    assert!(applied
        .warnings
        .iter()
        .any(|w| w.message.contains("no longer used")));
    assert_eq!(applied.settings_update, None);
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
