mod common;

use common::{client, entry, entry_with, policy, req, set, MIN};
use lr_core::{
    Condition, GroupBy, Policy, QueueEntry, Quota, Reject, Reserve, Rule, ServeLog, Strategy,
    Timestamp, Wait, Zone, DEFAULT_AGE_AFTER_MIN, DEFAULT_CI_PRIORITY,
};

fn ids(queue: &[QueueEntry], order: Vec<usize>) -> Vec<String> {
    order
        .into_iter()
        .map(|i| queue[i].order.job.0.clone())
        .collect()
}

fn ordered(p: &dyn Policy, queue: &[QueueEntry], now_ms: u64) -> Vec<String> {
    ids(
        queue,
        p.order(queue, &ServeLog::default(), Timestamp(now_ms)),
    )
}

fn rule(name: Option<&str>, label: &str) -> Rule {
    Rule {
        name: name.map(str::to_string),
        when: Condition {
            client_label: Some(label.into()),
            ..Condition::default()
        },
        ..Rule::default()
    }
}

#[test]
fn default_policy_matches_spec_defaults() {
    let p = policy(8192, 8);
    assert_eq!(p.strategy, Strategy::PriorityFifo);
    assert_eq!(p.age_after_min, DEFAULT_AGE_AFTER_MIN);
    assert_eq!(DEFAULT_AGE_AFTER_MIN, 30);
    assert_eq!(p.group_by, GroupBy::Label);
    assert_eq!(p.max_job_timeout_s, None);
    assert_eq!(
        p.runner_zone_order,
        vec![Zone::Local, Zone::Lan, Zone::Remote]
    );
    assert_eq!(p.priority_of(&entry("c", "ci", 0)), DEFAULT_CI_PRIORITY);
    assert_eq!(DEFAULT_CI_PRIORITY, 100);
    assert_eq!(p.priority_of(&entry("a", "agent", 0)), 0);
}

#[test]
fn strict_priority_puts_ci_before_older_orders() {
    let queue = [entry("old", "agent", 0), entry("ci", "ci", 5_000)];
    assert_eq!(ordered(&policy(8192, 8), &queue, 10_000), ["ci", "old"]);
}

#[test]
fn fifo_within_priority() {
    let queue = [
        entry("third", "agent", 3_000),
        entry("first", "agent", 1_000),
        entry("second", "guest", 2_000),
    ];
    assert_eq!(
        ordered(&policy(8192, 8), &queue, 10_000),
        ["first", "second", "third"]
    );
}

#[test]
fn wanted_priority_is_only_a_wish() {
    let mut wish = entry("wish", "agent", 2_000);
    wish.order.wanted_priority = Some(1_000);
    let queue = [wish, entry("plain", "agent", 1_000)];
    assert_eq!(ordered(&policy(8192, 8), &queue, 3_000), ["plain", "wish"]);
}

#[test]
fn aging_promotes_after_threshold_and_survives_rebroker() {
    let p = policy(8192, 8);
    let mut old = entry("old", "agent", 0);
    old.accepted_at = Timestamp(30 * MIN + 1);
    let ci = entry("ci", "ci", 10 * MIN);
    let queue = [ci, old];
    assert_eq!(ordered(&p, &queue, 30 * MIN), ["ci", "old"]);
    assert_eq!(p.effective_priority(&queue[1], Timestamp(30 * MIN)), 0);
    assert_eq!(ordered(&p, &queue, 30 * MIN + 1), ["old", "ci"]);
    assert_eq!(
        p.effective_priority(&queue[1], Timestamp(30 * MIN + 1)),
        DEFAULT_CI_PRIORITY
    );
}

#[test]
fn aging_threshold_follows_age_after_min() {
    let mut p = policy(8192, 8);
    p.age_after_min = 5;
    let queue = [entry("ci", "ci", 4 * MIN), entry("old", "agent", 0)];
    assert_eq!(ordered(&p, &queue, 5 * MIN + 1), ["old", "ci"]);
}

#[test]
fn aging_raises_to_highest_defined_priority_only() {
    let mut p = policy(8192, 8);
    p.rules.push(Rule {
        priority: Some(500),
        ..rule(Some("vip"), "vip")
    });
    assert_eq!(p.max_priority(), 500);
    let queue = [entry("vip", "vip", 40 * MIN), entry("old", "agent", 0)];
    let order = ordered(&p, &queue, 40 * MIN + 1);
    assert_eq!(order, ["old", "vip"]);
    assert_eq!(
        p.effective_priority(&queue[1], Timestamp(40 * MIN + 1)),
        500
    );
}

#[test]
fn first_matching_rule_wins_per_field_and_fields_combine() {
    let mut p = policy(8192, 8);
    p.rules = vec![
        Rule {
            weight: Some(3),
            ..rule(Some("w"), "agent")
        },
        Rule {
            priority: Some(10),
            ..rule(Some("p10"), "agent")
        },
        Rule {
            priority: Some(20),
            ..rule(Some("p20"), "agent")
        },
    ];
    let e = entry("a", "agent", 0);
    assert_eq!(p.priority_of(&e), 10);
    assert_eq!(p.weight_of(&e), 3);
}

#[test]
fn rule_needs_every_condition_field() {
    let mut p = policy(8192, 8);
    p.rules = vec![Rule {
        priority: Some(50),
        when: Condition {
            client_label: Some("ci".into()),
            source: Some("github".into()),
            ..Condition::default()
        },
        ..Rule::default()
    }];
    let mut gh = entry("gh", "ci", 0);
    gh.order.declared.source = Some("github".into());
    let mut session = entry("s", "ci", 0);
    session.order.declared.source = Some("session".into());
    let mut forged = entry("f", "agent", 0);
    forged.order.declared.source = Some("github".into());
    assert_eq!(p.priority_of(&gh), 50);
    assert_eq!(p.priority_of(&session), 0);
    assert_eq!(p.priority_of(&forged), 0);
    assert!(p.rules[0].when.uses_declared());
    assert!(!rule(None, "ci").when.uses_declared());
}

#[test]
fn conditions_match_tag_size_project_and_trusted_fields() {
    let mut r = req(8192, 2, 60);
    r.tags = set(&["emulator"]);
    let mut e = entry_with("x", "ci", 0, r);
    e.order.declared.project = Some("o/r".into());
    e.order.declared.label = Some("agent-1".into());
    let full = Condition {
        client_label: Some("ci".into()),
        client_key: Some("key-ci".into()),
        machine_id: Some("m-client".into()),
        project: Some("o/r".into()),
        submitter_label: Some("agent-1".into()),
        tag: Some("emulator".into()),
        min_mem_mb: Some(8192),
        ..Condition::default()
    };
    assert!(full.matches(&e.order, &e.client));
    for broken in [
        Condition {
            tag: Some("gradle".into()),
            ..full.clone()
        },
        Condition {
            min_mem_mb: Some(8193),
            ..full.clone()
        },
        Condition {
            machine_id: Some("m-other".into()),
            ..full.clone()
        },
    ] {
        assert!(!broken.matches(&e.order, &e.client));
    }
}

#[test]
fn unknown_strategy_name_is_refused() {
    assert_eq!(
        "priority-fifo".parse::<Strategy>(),
        Ok(Strategy::PriorityFifo)
    );
    assert_eq!("fair-share".parse::<Strategy>(), Ok(Strategy::FairShare));
    assert_eq!("weighted".parse::<Strategy>(), Ok(Strategy::Weighted));
    assert!("round-robin".parse::<Strategy>().is_err());
    assert_eq!("key".parse::<GroupBy>(), Ok(GroupBy::Key));
    assert!("host".parse::<GroupBy>().is_err());
}

#[test]
fn unnamed_rule_is_named_by_its_number() {
    let mut p = policy(8192, 8);
    p.rules.push(Rule {
        reserve: Some(Reserve { cpu: 2, mem_mb: 0 }),
        ..rule(None, "ci")
    });
    assert_eq!(p.rule_name(0), "ci-first");
    assert_eq!(p.rule_name(1), "2");
}

#[test]
fn timeout_above_limit_is_rejected_at_intake() {
    let mut p = policy(8192, 8);
    p.max_job_timeout_s = Some(3600);
    let e = entry_with("x", "agent", 0, req(1024, 1, 7200));
    let err = p.admit(&e.order, &e.client).unwrap_err();
    assert_eq!(
        err,
        Reject::Timeout {
            needed: 7200,
            ceiling: 3600
        }
    );
    assert_eq!(err.to_string(), "потолок таймаута 3600 с, нужно 7200");
    let ok = entry_with("y", "agent", 0, req(1024, 1, 3600));
    assert!(p.admit(&ok.order, &ok.client).is_ok());
}

#[test]
fn order_larger_than_pool_is_rejected_at_intake() {
    let p = policy(8192, 8);
    let e = entry_with("x", "agent", 0, req(12288, 1, 60));
    assert!(matches!(
        p.admit(&e.order, &e.client),
        Err(Reject::PoolMemory { .. })
    ));
}

fn ci_reserve() -> lr_core::RulePolicy {
    let mut p = policy(16384, 8);
    p.rules.push(Rule {
        reserve: Some(Reserve {
            cpu: 2,
            mem_mb: 4096,
        }),
        ..rule(Some("ci-reserve"), "ci")
    });
    p
}

#[test]
fn reserve_is_kept_from_other_orders_even_when_free() {
    let p = ci_reserve();
    let agent = entry_with("a", "agent", 0, req(1024, 7, 60));
    let err = p.gate(&agent, &[]).unwrap_err();
    assert!(matches!(err, Wait::Reserved { ref rule, cpu: 2, .. } if rule == "ci-reserve"));
    assert_eq!(
        err.to_string(),
        "правило ci-reserve: 2 из 8 ядер и 4096 из 16384 МБ зарезервированы"
    );
    let cpu_only = Wait::Reserved {
        rule: "ci-reserve".into(),
        cpu: 2,
        pool_cpu: 8,
        mem_mb: 0,
        pool_mem_mb: 16384,
    };
    assert_eq!(
        cpu_only.to_string(),
        "правило ci-reserve: 2 из 8 ядер зарезервированы"
    );
    let ci = entry_with("c", "ci", 0, req(1024, 8, 60));
    assert!(p.gate(&ci, &[]).is_ok());
    let fits = entry_with("b", "agent", 0, req(1024, 6, 60));
    assert!(p.gate(&fits, &[]).is_ok());
}

#[test]
fn reserve_consumed_by_its_orders_no_longer_blocks() {
    let p = ci_reserve();
    let ci = entry_with("c", "ci", 0, req(4096, 2, 60));
    let agent = entry_with("a", "agent", 0, req(1024, 6, 60));
    assert!(p.gate(&agent, &[&ci]).is_ok());
}

#[test]
fn order_that_only_fits_into_a_foreign_reserve_is_rejected() {
    let p = ci_reserve();
    let agent = entry_with("a", "agent", 0, req(14000, 1, 60));
    assert!(matches!(
        p.admit(&agent.order, &agent.client),
        Err(Reject::Reserved { ref rule, .. }) if rule == "ci-reserve"
    ));
    let ci = entry_with("c", "ci", 0, req(14000, 1, 60));
    assert!(p.admit(&ci.order, &ci.client).is_ok());
}

fn quota_policy(group_by: GroupBy) -> lr_core::RulePolicy {
    let mut p = policy(65536, 32);
    p.group_by = group_by;
    p.rules.push(Rule {
        quota: Some(Quota {
            max_running: Some(2),
            max_mem_mb: Some(4096),
        }),
        ..rule(Some("agent-quota"), "agent")
    });
    p
}

#[test]
fn quota_caps_running_jobs_of_a_group() {
    let p = quota_policy(GroupBy::Label);
    let a = entry("a", "agent", 0);
    let b = entry("b", "agent", 0);
    let c = entry("c", "agent", 0);
    assert!(p.gate(&c, &[&a]).is_ok());
    let err = p.gate(&c, &[&a, &b]).unwrap_err();
    assert!(matches!(err, Wait::Quota { ref rule, running: 2, .. } if rule == "agent-quota"));
    let ci = entry("ci", "ci", 0);
    assert!(p.gate(&ci, &[&a, &b]).is_ok());
}

#[test]
fn quota_caps_memory_of_a_group() {
    let p = quota_policy(GroupBy::Label);
    let a = entry_with("a", "agent", 0, req(3072, 1, 60));
    let b = entry_with("b", "agent", 0, req(2048, 1, 60));
    assert!(matches!(
        p.gate(&b, &[&a]),
        Err(Wait::Quota { mem_mb: 3072, .. })
    ));
    let big = entry_with("big", "agent", 0, req(8192, 1, 60));
    assert!(matches!(
        p.admit(&big.order, &big.client),
        Err(Reject::Quota { .. })
    ));
}

#[test]
fn quota_groups_by_key_when_configured() {
    let p = quota_policy(GroupBy::Key);
    let a = entry("a", "agent", 0);
    let mut b = entry("b", "agent", 0);
    b.client.key = "other-key".into();
    let mut c = entry("c", "agent", 0);
    c.client.key = "third-key".into();
    assert!(p.gate(&c, &[&a, &b]).is_ok());
    assert_eq!(p.group_of(&client("agent")), "key-agent");
}

#[test]
fn declared_attributes_come_from_identity() {
    let id = lr_core::Identity {
        label: "agent-7".into(),
        note: None,
        os_user: "alex".into(),
        host: "pc".into(),
        sender_pid: 1,
        sender_start: None,
        cwd: "/w".into(),
        branch: None,
        worktree_root: None,
        repo_root: None,
        project: "owner/repo".into(),
        source: lr_core::Source::Session,
    };
    let d = lr_core::Declared::from_identity(&id);
    assert_eq!(d.source.as_deref(), Some("session"));
    assert_eq!(d.project.as_deref(), Some("owner/repo"));
    assert_eq!(d.label.as_deref(), Some("agent-7"));
}
