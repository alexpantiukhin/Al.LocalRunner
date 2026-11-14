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

fn tag(t: &str) -> Condition {
    Condition {
        tag: Some(t.into()),
        ..Condition::default()
    }
}

#[test]
fn declared_inside_any_and_not_is_found() {
    let not = Condition {
        not: Some(Box::new(project("x"))),
        ..Condition::default()
    };
    assert!(not.uses_declared());
    assert_eq!(not.declared_names(), vec!["project"]);
    let any = Condition {
        any: vec![tag("gpu")],
        ..Condition::default()
    };
    assert!(any.uses_declared());
    assert_eq!(any.declared_names(), vec!["tag"]);
    let num = num_term(NumAttr::MemMb, NumTest::Ge(1));
    assert!(num.uses_declared());
    assert_eq!(num.declared_names(), vec!["mem_mb"]);
    assert!(!label("ci").uses_declared());
    assert!(label("ci").declared_names().is_empty());
}

#[test]
fn guard_requires_positive_label_test() {
    assert!(label("ci").guarded_by_label());
    for t in [
        StrTest::In(vec!["a".into()]),
        StrTest::Prefix("a".into()),
        StrTest::Glob("a*".into()),
        StrTest::Eq("a".into()),
    ] {
        assert!(str_term(StrAttr::ClientLabel, t).guarded_by_label());
    }
    let any = |a: Vec<Condition>| Condition {
        any: a,
        ..Condition::default()
    };
    assert!(any(vec![label("a"), label("b")]).guarded_by_label());
    assert!(!any(vec![label("a"), project("p")]).guarded_by_label());
    assert!(!str_term(StrAttr::ClientLabel, StrTest::Ne("x".into())).guarded_by_label());
    let not = Condition {
        not: Some(Box::new(label("x"))),
        ..Condition::default()
    };
    assert!(!not.guarded_by_label());
    let key = Condition {
        client_key: Some("k".into()),
        ..Condition::default()
    };
    assert!(!key.guarded_by_label());
    assert!(!Condition::default().guarded_by_label());
}

#[test]
fn covers_is_conservative() {
    let both = Condition {
        client_label: Some("ci".into()),
        project: Some("p".into()),
        ..Condition::default()
    };
    assert!(label("ci").covers(&both));
    assert!(!label("ci").covers(&project("p")));
    let any = |a: Vec<Condition>| Condition {
        any: a,
        ..Condition::default()
    };
    let ab = any(vec![label("a"), label("b")]);
    assert!(ab.covers(&ab));
    assert!(ab.covers(&any(vec![label("a")])));
    assert!(!ab.covers(&any(vec![label("a"), label("b"), label("c")])));
    assert!(!ab.covers(&Condition::default()));
    let ge = |n| num_term(NumAttr::MemMb, NumTest::Ge(n));
    assert!(ge(1024).covers(&ge(1024)));
    assert!(!ge(1024).covers(&ge(4096)));
    let not = Condition {
        not: Some(Box::new(project("a"))),
        ..Condition::default()
    };
    assert!(!not.covers(&project("b")));
    assert!(not.covers(&not));
    let min = |n| Condition {
        min_mem_mb: Some(n),
        ..Condition::default()
    };
    assert!(min(1024).covers(&min(2048)));
    assert!(!min(2048).covers(&min(1024)));
}

#[test]
fn describe_names_every_part() {
    assert_eq!(label("ci").describe(), "client_label = \"ci\"");
    assert_eq!(Condition::default().describe(), "everyone");
    let cond = Condition {
        client_label: Some("ci".into()),
        min_mem_mb: Some(8192),
        terms: vec![
            Term::Str(StrAttr::Project, StrTest::In(vec!["a".into(), "b".into()])),
            Term::Str(StrAttr::Source, StrTest::Prefix("g".into())),
            Term::Str(StrAttr::Tag, StrTest::Glob("x*".into())),
            Term::Str(StrAttr::Platform, StrTest::Ne("mac".into())),
            Term::Num(NumAttr::Cpu, NumTest::Lt(4)),
            Term::Num(NumAttr::DiskMb, NumTest::In(vec![1, 2])),
            Term::Num(NumAttr::TimeoutS, NumTest::Le(9)),
            Term::Num(NumAttr::MemMb, NumTest::Gt(1)),
            Term::Num(NumAttr::MemMb, NumTest::Ge(2)),
            Term::Num(NumAttr::MemMb, NumTest::Eq(3)),
            Term::Num(NumAttr::MemMb, NumTest::Ne(4)),
        ],
        any: vec![label("a"), project("p")],
        not: Some(Box::new(tag("t"))),
        ..Condition::default()
    };
    assert_eq!(
        cond.describe(),
        "client_label = \"ci\", mem_mb >= 8192, project in [\"a\", \"b\"], source prefix \"g\", \
         tag glob \"x*\", platform != \"mac\", cpu < 4, disk_mb in [1, 2], timeout_s <= 9, \
         mem_mb > 1, mem_mb >= 2, mem_mb = 3, mem_mb != 4, \
         any of (client_label = \"a\" | project = \"p\"), not (tag = \"t\")"
    );
}
