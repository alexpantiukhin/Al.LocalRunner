mod common;

use common::{entry, err, ok, HOST};
use lr_broker::policy::rule_policy;
use lr_core::{NumAttr, NumTest, Policy, ServeLog, StrAttr, StrTest, Term, Timestamp};

fn when(text: &str) -> lr_core::Condition {
    ok(&format!("[[rule]]\nwhen = {text}\nname = \"r\"\n"))
        .rules
        .remove(0)
        .rule
        .when
}

fn refused(text: &str) -> lr_broker::policy::PolicyError {
    err(&format!("[[rule]]\nwhen = {text}\npriority = 1\n"))
}

#[test]
fn flat_form_unchanged() {
    let w = when(r#"{ client_label = "ci", source = "github" }"#);
    assert_eq!(w.client_label.as_deref(), Some("ci"));
    assert_eq!(w.source.as_deref(), Some("github"));
    assert!(w.terms.is_empty());
    assert!(w.any.is_empty());
    assert!(w.not.is_none());
}

#[test]
fn in_prefix_glob_parse() {
    let w = when(
        r#"{ client_label = "ci", project = { in = ["a", "b"] }, submitter_label = { prefix = "claude:" }, source = { glob = "git*" } }"#,
    );
    assert_eq!(w.client_label.as_deref(), Some("ci"));
    assert_eq!(
        w.terms,
        vec![
            Term::Str(StrAttr::Source, StrTest::Glob("git*".into())),
            Term::Str(StrAttr::Project, StrTest::In(vec!["a".into(), "b".into()])),
            Term::Str(StrAttr::SubmitterLabel, StrTest::Prefix("claude:".into())),
        ]
    );
}

#[test]
fn numeric_tables_parse() {
    let w = when(
        r#"{ client_label = "ci", mem_mb = { ge = 1024, lt = 8192 }, cpu = 4, min_mem_mb = 2048 }"#,
    );
    assert_eq!(w.min_mem_mb, Some(2048));
    assert_eq!(
        w.terms,
        vec![
            Term::Num(NumAttr::MemMb, NumTest::Lt(8192)),
            Term::Num(NumAttr::MemMb, NumTest::Ge(1024)),
            Term::Num(NumAttr::Cpu, NumTest::Eq(4)),
        ]
    );
}

#[test]
fn any_and_not_parse() {
    let w = when(
        r#"{ any = [{ client_label = "ci" }, { client_label = "night" }], not = { client_label = "x" } }"#,
    );
    assert_eq!(w.any.len(), 2);
    assert_eq!(w.any[1].client_label.as_deref(), Some("night"));
    assert_eq!(
        w.not.as_ref().and_then(|n| n.client_label.as_deref()),
        Some("x")
    );
}

#[test]
fn empty_forms_rejected() {
    for text in [
        "{ any = [] }",
        "{ any = [{}] }",
        "{ not = {} }",
        r#"{ project = { in = [] } }"#,
        "{ mem_mb = {} }",
        "{ mem_mb = { in = [] } }",
        "{ project = {} }",
    ] {
        let e = refused(text);
        assert_eq!(e.table.as_deref(), Some("rule"), "{text}");
        assert_eq!(e.key.as_deref(), Some("when"), "{text}");
        assert_eq!(e.line, Some(1), "{text}");
    }
}

#[test]
fn bad_values_rejected() {
    for text in [
        "{ mem_mb = -1 }",
        "{ cpu = 1.5 }",
        r#"{ project = { unknown = "x" } }"#,
        "{ project = 5 }",
        r#"{ zone = "lan" }"#,
    ] {
        let e = refused(text);
        assert_eq!(e.table.as_deref(), Some("rule"), "{text}: {e}");
        assert!(e.line.is_some(), "{text}: {e}");
    }
    assert!(refused(r#"{ project = { unknown = "x" } }"#)
        .message
        .contains("unknown"));
    assert!(refused("{ project = 5 }").message.contains("prefix"));
}

#[test]
fn trust_rule_over_the_tree() {
    for text in [
        r#"{ project = "p" }"#,
        r#"{ any = [{ client_label = "ci" }, { project = "p" }] }"#,
        r#"{ not = { project = "p" } }"#,
        "{ mem_mb = { ge = 1 } }",
        r#"{ client_label = { ne = "x" }, project = "p" }"#,
        r#"{ not = { client_label = "x" }, project = "p" }"#,
    ] {
        let e = refused(text);
        assert_eq!(e.key.as_deref(), Some("when"), "{text}");
        assert!(e.message.contains("client_label"), "{text}: {e}");
    }
    for text in [
        r#"{ client_label = { in = ["a", "b"] }, mem_mb = { ge = 8192 } }"#,
        r#"{ any = [{ client_label = "a", project = "p" }, { client_label = "b" }] }"#,
        r#"{ client_label = { prefix = "ci" }, not = { project = "p" } }"#,
    ] {
        ok(&format!("[[rule]]\nwhen = {text}\npriority = 1\n"));
    }
    ok("[[rule]]\nname = \"n\"\nwhen = { project = \"p\" }\n");
}

#[test]
fn negative_priority_allowed() {
    let f = ok(r#"
[[rule]]
name = "bulk"
when = { client_label = "bulk" }
priority = -5
"#);
    assert_eq!(f.rules[0].rule.priority, Some(-5));
    let p = rule_policy(&f, &HOST);
    let queue = vec![entry("a-bulk", "bulk", 0), entry("z-plain", "agent", 10)];
    assert_eq!(
        p.order(&queue, &ServeLog::default(), Timestamp(20)),
        vec![1, 0]
    );
}
