mod common;

use common::{client, entry, entry_with, req, set};
use lr_core::{glob_match, ClientView, Condition, NumAttr, NumTest, Order, StrAttr, StrTest, Term};

fn order() -> Order {
    entry("j", "ci", 0).order
}

fn with_project(project: Option<&str>) -> Order {
    let mut o = order();
    o.declared.project = project.map(str::to_string);
    o
}

fn str_term(attr: StrAttr, test: StrTest) -> Condition {
    Condition {
        terms: vec![Term::Str(attr, test)],
        ..Condition::default()
    }
}

fn num_term(attr: NumAttr, test: NumTest) -> Condition {
    Condition {
        terms: vec![Term::Num(attr, test)],
        ..Condition::default()
    }
}

fn label(l: &str) -> Condition {
    Condition {
        client_label: Some(l.into()),
        ..Condition::default()
    }
}

fn project(p: &str) -> Condition {
    Condition {
        project: Some(p.into()),
        ..Condition::default()
    }
}

fn c(label: &str) -> ClientView {
    client(label)
}

#[test]
fn flat_form_still_matches_by_equality() {
    assert!(label("ci").matches(&order(), &c("ci")));
    assert!(!label("ci").matches(&order(), &c("agent")));
}

#[test]
fn in_matches_any_listed_value() {
    let cond = str_term(
        StrAttr::ClientLabel,
        StrTest::In(vec!["ci".into(), "night".into()]),
    );
    assert!(cond.matches(&order(), &c("night")));
    assert!(!cond.matches(&order(), &c("agent")));
}

#[test]
fn prefix_and_glob() {
    let prefix = str_term(StrAttr::Project, StrTest::Prefix("lr-".into()));
    assert!(prefix.matches(&with_project(Some("lr-core")), &c("ci")));
    assert!(!prefix.matches(&with_project(Some("core-lr")), &c("ci")));
    let glob = str_term(StrAttr::Project, StrTest::Glob("o/*-api".into()));
    assert!(glob.matches(&with_project(Some("o/x-api")), &c("ci")));
    assert!(!glob.matches(&with_project(Some("o/x-apis")), &c("ci")));
    assert!(glob_match("a?c", "abc"));
    assert!(!glob_match("a?c", "ac"));
    assert!(glob_match("*", ""));
    assert!(!glob_match("A*", "abc"));
    assert!(glob_match("a*b*c", "aXbYbZc"));
}

fn sized(attr: NumAttr, value: u64) -> Order {
    let mut r = req(1, 1, 1);
    match attr {
        NumAttr::MemMb => r.mem_mb = value,
        NumAttr::Cpu => r.cpu = value as u32,
        NumAttr::DiskMb => r.disk_mb = value,
        NumAttr::TimeoutS => r.timeout_s = value,
    }
    entry_with("j", "ci", 0, r).order
}

#[test]
fn numeric_comparisons() {
    for attr in [
        NumAttr::MemMb,
        NumAttr::Cpu,
        NumAttr::DiskMb,
        NumAttr::TimeoutS,
    ] {
        let o = sized(attr, 4096);
        let m = |t: NumTest| num_term(attr, t).matches(&o, &c("ci"));
        assert!(m(NumTest::Ge(4096)));
        assert!(!m(NumTest::Gt(4096)));
        assert!(!m(NumTest::Le(4095)));
        assert!(m(NumTest::Lt(4097)));
        assert!(!m(NumTest::Ne(4096)));
        assert!(m(NumTest::In(vec![1024, 4096])));
        assert!(m(NumTest::Eq(4096)));
    }
}

#[test]
fn legacy_min_mem_mb_is_greater_or_equal() {
    let cond = Condition {
        min_mem_mb: Some(4096),
        ..Condition::default()
    };
    assert!(cond.matches(&sized(NumAttr::MemMb, 4096), &c("ci")));
    assert!(!cond.matches(&sized(NumAttr::MemMb, 4095), &c("ci")));
}

#[test]
fn any_not_compose() {
    let any = Condition {
        any: vec![label("ci"), project("x")],
        ..Condition::default()
    };
    assert!(any.matches(&order(), &c("ci")));
    assert!(any.matches(&with_project(Some("x")), &c("agent")));
    assert!(!any.matches(&with_project(Some("y")), &c("agent")));
    let not = Condition {
        not: Some(Box::new(project("x"))),
        ..Condition::default()
    };
    assert!(!not.matches(&with_project(Some("x")), &c("ci")));
    assert!(not.matches(&with_project(Some("y")), &c("ci")));
    let both = Condition {
        client_label: Some("ci".into()),
        any: vec![project("x"), project("y")],
        ..Condition::default()
    };
    assert!(both.matches(&with_project(Some("x")), &c("ci")));
    assert!(!both.matches(&with_project(Some("x")), &c("agent")));
}

#[test]
fn missing_value_fails_positive_tests_and_passes_ne() {
    let none = with_project(None);
    for t in [
        StrTest::Eq("x".into()),
        StrTest::In(vec!["x".into()]),
        StrTest::Prefix("x".into()),
        StrTest::Glob("x*".into()),
    ] {
        assert!(!str_term(StrAttr::Project, t).matches(&none, &c("ci")));
    }
    assert!(str_term(StrAttr::Project, StrTest::Ne("x".into())).matches(&none, &c("ci")));
    let mut no_platform = order();
    no_platform.requirements.platform = None;
    assert!(
        !str_term(StrAttr::Platform, StrTest::Eq("linux".into())).matches(&no_platform, &c("ci"))
    );
    assert!(
        str_term(StrAttr::Platform, StrTest::Ne("linux".into())).matches(&no_platform, &c("ci"))
    );
}

#[test]
fn tag_is_multi_valued() {
    let mut o = order();
    o.requirements.tags = set(&["gpu"]);
    o.requirements.exclusive = set(&["db"]);
    let t = |t: StrTest| str_term(StrAttr::Tag, t).matches(&o, &c("ci"));
    assert!(t(StrTest::Eq("db".into())));
    assert!(t(StrTest::Prefix("g".into())));
    assert!(!t(StrTest::Ne("gpu".into())));
    assert!(t(StrTest::Ne("zzz".into())));
}

#[test]
fn empty_condition_matches_everyone() {
    assert!(Condition::default().matches(&order(), &c("anyone")));
}
