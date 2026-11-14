mod common;

use common::{entry, entry_with, err, ok, req, HOST};
use lr_broker::policy::{into_policy, rule_policy};
use lr_core::{Quota, Reject, Reserve};

#[test]
fn every_condition_and_action_field_maps_to_the_core_rule() {
    let f = ok(r#"
[[rule]]
name = "all"
when = { client_label = "ci", client_key = "k1", machine_id = "m1", source = "github", project = "o/r", submitter_label = "claude:1", tag = "gpu", min_mem_mb = 2048 }
priority = 7
reserve = { cpu = 1, mem_mb = 1024 }
quota = { max_running = 2, max_mem_mb = 8192 }
weight = 3
"#);
    let r = &f.rules[0].rule;
    let w = &r.when;
    assert_eq!(w.client_label.as_deref(), Some("ci"));
    assert_eq!(w.client_key.as_deref(), Some("k1"));
    assert_eq!(w.machine_id.as_deref(), Some("m1"));
    assert_eq!(w.source.as_deref(), Some("github"));
    assert_eq!(w.project.as_deref(), Some("o/r"));
    assert_eq!(w.submitter_label.as_deref(), Some("claude:1"));
    assert_eq!(w.tag.as_deref(), Some("gpu"));
    assert_eq!(w.min_mem_mb, Some(2048));
    assert_eq!(r.priority, Some(7));
    assert_eq!(
        r.reserve,
        Some(Reserve {
            cpu: 1,
            mem_mb: 1024
        })
    );
    assert_eq!(
        r.quota,
        Some(Quota {
            max_running: Some(2),
            max_mem_mb: Some(8192)
        })
    );
    assert_eq!(r.weight, Some(3));
}

#[test]
fn rule_line_is_recorded() {
    let f = ok("[queue]\n\n[[rule]]\npriority = 1\n\n[[rule]]\nname = \"b\"\npriority = 2\n");
    assert_eq!(f.rules[0].line, Some(3));
    assert_eq!(f.rules[1].line, Some(6));
}

#[test]
fn rules_keep_file_order_first_matching_rule_wins() {
    let f = ok(r#"
[[rule]]
when = { client_label = "ci" }
priority = 10

[[rule]]
priority = 1
"#);
    let p = rule_policy(&f, &HOST);
    assert_eq!(p.priority_of(&entry("a", "ci", 0)), 10);
    assert_eq!(p.priority_of(&entry("b", "agent", 0)), 1);
}

#[test]
fn rule_without_when_matches_every_order() {
    let f = ok("[[rule]]\npriority = 4\n");
    assert_eq!(
        rule_policy(&f, &HOST).priority_of(&entry("x", "anyone", 0)),
        4
    );
}

#[test]
fn declared_condition_without_trusted_label_refused() {
    for action in [
        "priority = 1",
        "reserve = { cpu = 1 }",
        "quota = { max_running = 1 }",
        "weight = 2",
    ] {
        let text = format!("[[rule]]\nname = \"r\"\nwhen = {{ source = \"github\" }}\n{action}\n");
        let e = err(&text);
        assert_eq!(e.table.as_deref(), Some("rule"), "{action}");
        assert_eq!(e.key.as_deref(), Some("when"), "{action}");
        assert_eq!(e.line, Some(1), "{action}");
        assert!(e.message.contains("client_label"), "{e}");
        assert!(e.message.contains("rule r"), "{e}");
        assert!(e.message.contains("source"), "{e}");
    }
}

#[test]
fn every_declared_field_needs_the_trusted_label() {
    for cond in [
        "source = \"github\"",
        "project = \"o/r\"",
        "submitter_label = \"x\"",
        "tag = \"gpu\"",
        "label = \"work\"",
        "label = { in = [\"work\", \"release\"] }",
        "min_mem_mb = 1",
    ] {
        let e = err(&format!("[[rule]]\nwhen = {{ {cond} }}\npriority = 1\n"));
        assert_eq!(e.key.as_deref(), Some("when"), "{cond}");
        let fine = format!("[[rule]]\nwhen = {{ client_label = \"ci\", {cond} }}\npriority = 1\n");
        ok(&fine);
    }
}

#[test]
fn trusted_only_conditions_need_no_label() {
    ok("[[rule]]\nwhen = { client_key = \"k\" }\npriority = 1\n");
    ok("[[rule]]\nwhen = { machine_id = \"m\" }\npriority = 1\n");
}

#[test]
fn duplicate_rule_names_refused() {
    let e = err("[[rule]]\nname = \"a\"\npriority = 1\n[[rule]]\nname = \"a\"\npriority = 2\n");
    assert_eq!(e.table.as_deref(), Some("rule"));
    assert_eq!(e.key.as_deref(), Some("name"));
    assert_eq!(e.line, Some(4));
}

#[test]
fn zero_weight_refused() {
    let e = err("[[rule]]\nweight = 0\n");
    assert_eq!(e.key.as_deref(), Some("weight"));
    assert_eq!(e.line, Some(2));
}

#[test]
fn misspelled_condition_field_refused() {
    let e = err("[[rule]]\nwhen = { client_lable = \"ci\" }\npriority = 1\n");
    assert_eq!(e.table.as_deref(), Some("rule"));
    assert_eq!(e.key.as_deref(), Some("client_lable"));
    assert_eq!(e.line, Some(2));
}

#[test]
fn misspelled_action_refused() {
    let e = err("[[rule]]\npriorty = 1\n");
    assert_eq!(e.key.as_deref(), Some("priorty"));
    let e = err("[[rule]]\nreserve = { cpus = 1 }\n");
    assert_eq!(e.key.as_deref(), Some("cpus"));
}

#[test]
fn rejection_names_rule_by_name_or_number() {
    let named = ok(r#"
[pool]
mem_mb = 8192
cpu = 8

[[rule]]
name = "ci-reserve"
when = { client_label = "ci" }
reserve = { cpu = 6 }
"#);
    let p = into_policy(&named, &HOST);
    let big = entry_with("g", "agent", 0, req(1024, 4));
    match p.admit(&big.order, &big.client) {
        Err(Reject::Reserved { rule, .. }) => assert_eq!(rule, "ci-reserve"),
        other => panic!("{other:?}"),
    }
    let unnamed = ok(r#"
[pool]
mem_mb = 8192
cpu = 8

[[rule]]
priority = 1

[[rule]]
when = { client_label = "ci" }
reserve = { cpu = 6 }
"#);
    let p = into_policy(&unnamed, &HOST);
    match p.admit(&big.order, &big.client) {
        Err(Reject::Reserved { rule, .. }) => assert_eq!(rule, "2"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn pool_and_limits_reach_the_core_policy() {
    let f = ok(r#"
[pool]
mem_mb = 4096
cpu = 2
exclusive_tags = ["emu"]

[limits]
max_job_timeout_s = 100

[clients]
group_by = "key"

[routing]
runner_zone_order = ["remote", "local"]
"#);
    let p = rule_policy(&f, &HOST);
    assert_eq!((p.pool.mem_mb, p.pool.cpu), (4096, 2));
    assert!(p.pool.exclusive_tags.contains("emu"));
    assert_eq!(p.max_job_timeout_s, Some(100));
    assert_eq!(p.group_by, lr_core::GroupBy::Key);
    assert_eq!(
        p.runner_zone_order,
        [lr_core::Zone::Remote, lr_core::Zone::Local]
    );
}
