mod common;

use common::{ok, HOST, SPEC_EXAMPLE};
use lr_broker::policy::{check, PolicyWarning};

fn warnings(text: &str) -> Vec<PolicyWarning> {
    check(&ok(text), &HOST)
}

fn has(ws: &[PolicyWarning], needle: &str) -> bool {
    ws.iter().any(|w| w.message.contains(needle))
}

#[test]
fn fully_shadowed_rule_warned_with_its_line() {
    let ws = warnings(
        "[[rule]]\nname = \"all\"\npriority = 1\n\n[[rule]]\nname = \"ci\"\nwhen = { client_label = \"ci\" }\npriority = 5\n",
    );
    let w = ws
        .iter()
        .find(|w| w.message.contains("never"))
        .expect("shadow warning");
    assert!(w.message.contains("rule ci"), "{w:?}");
    assert!(w.message.contains("all"), "{w:?}");
    assert_eq!(w.table.as_deref(), Some("rule"));
    assert_eq!(w.line, Some(5));
}

#[test]
fn rule_with_another_field_is_not_shadowed() {
    let ws = warnings(
        "[[rule]]\npriority = 1\n\n[[rule]]\nwhen = { client_label = \"ci\" }\npriority = 5\nweight = 2\n",
    );
    assert!(!has(&ws, "never"), "{ws:?}");
}

#[test]
fn narrower_earlier_rule_does_not_shadow() {
    let ws = warnings(
        "[[rule]]\nwhen = { client_label = \"ci\", source = \"github\" }\npriority = 1\n\n[[rule]]\nwhen = { client_label = \"ci\" }\npriority = 5\n",
    );
    assert!(!has(&ws, "never"), "{ws:?}");
}

#[test]
fn fields_covered_by_different_earlier_rules_shadow_together() {
    let ws = warnings(
        "[[rule]]\npriority = 1\n\n[[rule]]\nweight = 2\n\n[[rule]]\nwhen = { client_label = \"ci\" }\npriority = 5\nweight = 3\n",
    );
    assert!(has(&ws, "never"), "{ws:?}");
}

#[test]
fn smaller_memory_condition_shadows_larger_one() {
    let ws = warnings(
        "[[rule]]\nwhen = { client_label = \"ci\", min_mem_mb = 1024 }\npriority = 1\n\n[[rule]]\nwhen = { client_label = \"ci\", min_mem_mb = 4096 }\npriority = 2\n",
    );
    assert!(has(&ws, "never"), "{ws:?}");
}

#[test]
fn spec_example_rules_are_not_shadowed() {
    let ws = check(&ok(SPEC_EXAMPLE), &HOST);
    assert!(!has(&ws, "never"), "{ws:?}");
}

#[test]
fn rule_without_action_warned() {
    let ws = warnings("[[rule]]\nname = \"idle\"\nwhen = { client_label = \"ci\" }\n");
    assert!(has(&ws, "sets no"), "{ws:?}");
}

#[test]
fn memory_condition_above_the_pool_warned() {
    let ws = warnings(
        "[pool]\nmem_mb = 8192\ncpu = 4\n\n[[rule]]\nwhen = { client_label = \"ci\", min_mem_mb = 9000 }\npriority = 1\n",
    );
    assert!(has(&ws, "min_mem_mb"), "{ws:?}");
}

#[test]
fn reserves_covering_the_pool_warned() {
    let ws = warnings(
        "[pool]\nmem_mb = 8192\ncpu = 4\n\n[[rule]]\nwhen = { client_label = \"ci\" }\nreserve = { cpu = 2 }\n\n[[rule]]\nwhen = { client_label = \"night\" }\nreserve = { cpu = 2 }\n",
    );
    assert!(has(&ws, "reserve"), "{ws:?}");
    let fine = warnings(
        "[pool]\nmem_mb = 8192\ncpu = 4\n\n[[rule]]\nwhen = { client_label = \"ci\" }\nreserve = { cpu = 2 }\n",
    );
    assert!(!has(&fine, "reserve"), "{fine:?}");
}

#[test]
fn zero_running_quota_warned() {
    let ws =
        warnings("[[rule]]\nwhen = { client_label = \"guest\" }\nquota = { max_running = 0 }\n");
    assert!(has(&ws, "max_running = 0"), "{ws:?}");
}

#[test]
fn wide_network_range_warned() {
    for range in ["10.0.0.0/8", "0.0.0.0/0", "2001:db8::/32"] {
        let ws = warnings(&format!("[networks]\nall = [\"{range}\"]\n"));
        let w = ws
            .iter()
            .find(|w| w.table.as_deref() == Some("networks"))
            .unwrap_or_else(|| panic!("{range}: {ws:?}"));
        assert_eq!(w.key.as_deref(), Some("all"));
    }
    let ws = warnings("[networks]\nhome = [\"192.168.0.0/16\", \"fd00::/48\"]\n");
    assert!(
        ws.iter().all(|w| w.table.as_deref() != Some("networks")),
        "{ws:?}"
    );
}

#[test]
fn secrets_without_named_commands_warned() {
    let ws = warnings("[[secrets.allow]]\nlabel = \"ci\"\nnames = [\"key\"]\n");
    assert!(
        ws.iter()
            .any(|w| w.table.as_deref() == Some("secrets.allow")),
        "{ws:?}"
    );
    let named = warnings(
        "[[secrets.allow]]\nlabel = \"ci\"\nnames = [\"key\"]\n\n[[commands.client]]\nlabel = \"ci\"\nmode = \"named\"\n",
    );
    assert!(
        named
            .iter()
            .all(|w| w.table.as_deref() != Some("secrets.allow")),
        "{named:?}"
    );
    let all_named = warnings(
        "[[secrets.allow]]\nlabel = \"ci\"\nnames = [\"key\"]\n\n[commands]\ndefault = \"named\"\n",
    );
    assert!(all_named
        .iter()
        .all(|w| w.table.as_deref() != Some("secrets.allow")));
}

#[test]
fn shared_project_cache_warned() {
    let ws = warnings("[cache]\nshare = \"project\"\n");
    assert!(
        ws.iter().any(|w| w.table.as_deref() == Some("cache")),
        "{ws:?}"
    );
}

#[test]
fn full_visibility_warned() {
    let ws = warnings("[visibility]\nothers_orders = \"full\"\n");
    assert!(
        ws.iter().any(|w| w.table.as_deref() == Some("visibility")),
        "{ws:?}"
    );
}

#[test]
fn repeated_allow_label_warned() {
    let ws = warnings(
        "[[services.allow]]\nlabel = \"ci\"\nnames = [\"a\"]\n\n[[services.allow]]\nlabel = \"ci\"\nnames = [\"b\"]\n",
    );
    assert!(
        ws.iter()
            .any(|w| w.table.as_deref() == Some("services.allow")),
        "{ws:?}"
    );
}

#[test]
fn first_matching_allow_entry_decides() {
    let f = ok(
        "[[secrets.allow]]\nlabel = \"ci\"\nnames = [\"a\"]\n\n[[secrets.allow]]\nlabel = \"ci\"\nnames = [\"b\"]\n",
    );
    assert_eq!(f.secrets_allowed("ci"), ["a"]);
    assert!(f.secrets_allowed("agent").is_empty());
}

#[test]
fn first_matching_command_client_decides() {
    let f = ok(
        "[commands]\ndefault = \"any\"\n\n[[commands.client]]\nlabel = \"guest\"\nmode = \"named\"\n\n[[commands.client]]\nkey = \"k1\"\nmode = \"any\"\n",
    );
    assert_eq!(
        f.command_mode("k1", "guest"),
        lr_broker::policy::CommandMode::Named
    );
    assert_eq!(
        f.command_mode("k1", "agent"),
        lr_broker::policy::CommandMode::Any
    );
    assert_eq!(
        f.command_mode("k2", "agent"),
        lr_broker::policy::CommandMode::Any
    );
}

#[test]
fn default_file_has_no_warnings() {
    assert!(check(&ok(""), &HOST).is_empty());
}
