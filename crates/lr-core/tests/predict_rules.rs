mod common;

use common::{entry_with, job, pair, plan, policy, req, runner, running, started, wait_of, SEC};
use lr_core::{Condition, Quota, Reserve, Rule, RulePolicy, Timestamp, Wait};

fn label_rule(name: &str, label: &str) -> Rule {
    Rule {
        name: Some(name.into()),
        when: Condition {
            client_label: Some(label.into()),
            ..Condition::default()
        },
        ..Rule::default()
    }
}

fn quota_policy() -> RulePolicy {
    let mut p = policy(8192, 8);
    p.rules.push(Rule {
        quota: Some(Quota {
            max_running: Some(2),
            max_mem_mb: None,
        }),
        ..label_rule("agent-quota", "agent")
    });
    p
}

#[test]
fn filler_taking_the_heads_quota_slot_waits() {
    let p = quota_policy();
    let runners = [runner("r1"), runner("r2"), runner("r3"), runner("r4")];
    let run = [
        running(entry_with("a", "agent", 0, req(1024, 1, 600)), "r1", 0),
        running(entry_with("c", "ci", 0, req(6144, 1, 300)), "r2", 0),
    ];
    let queue = [
        entry_with("head", "agent", 1, req(4096, 1, 600)),
        entry_with("agent-long", "agent", 2, req(1024, 1, 3600)),
        entry_with("agent-short", "agent", 3, req(1024, 1, 100)),
    ];
    let out = plan(&p, &queue, &runners, &run, 100 * SEC);
    assert_eq!(out.reservation.as_ref().unwrap().at, Timestamp(300 * SEC));
    assert_eq!(
        wait_of(&out, "agent-long"),
        Some(&Wait::HeadReserved {
            head: job("head"),
            at: Timestamp(300 * SEC)
        })
    );
    assert_eq!(started(&out), [pair("agent-short", "r3")]);
}

#[test]
fn quota_moves_the_heads_reservation_later() {
    let p = quota_policy();
    let runners = [runner("r1"), runner("r2"), runner("r3")];
    let run = [
        running(entry_with("a1", "agent", 0, req(1024, 1, 300)), "r1", 0),
        running(entry_with("a2", "agent", 0, req(1024, 1, 900)), "r2", 0),
    ];
    let queue = [entry_with("head", "agent", 1, req(1024, 1, 600))];
    let out = plan(&p, &queue, &runners, &run, 100 * SEC);
    assert!(matches!(wait_of(&out, "head"), Some(Wait::Quota { .. })));
    assert_eq!(out.reservation.as_ref().unwrap().at, Timestamp(300 * SEC));
    let unlimited = policy(8192, 8);
    let out = plan(&unlimited, &queue, &runners, &run, 100 * SEC);
    assert_eq!(started(&out), [pair("head", "r3")]);
}

fn reserve_policy() -> RulePolicy {
    let mut p = policy(8192, 8);
    p.rules.push(Rule {
        reserve: Some(Reserve {
            cpu: 0,
            mem_mb: 2048,
        }),
        ..label_rule("ci-reserve", "ci")
    });
    p
}

fn reserve_case(filler: lr_core::QueueEntry) -> lr_core::Plan {
    let runners = [runner("r1"), runner("r2"), runner("r3")];
    let run = [running(
        entry_with("x", "agent", 0, req(4096, 1, 600)),
        "r1",
        0,
    )];
    let queue = [entry_with("head", "agent", 1, req(5120, 1, 600)), filler];
    plan(&reserve_policy(), &queue, &runners, &run, 100 * SEC)
}

#[test]
fn reserve_is_kept_out_of_the_heads_reservation() {
    let out = reserve_case(entry_with("small", "agent", 2, req(1024, 1, 3600)));
    assert_eq!(out.reservation.as_ref().unwrap().at, Timestamp(600 * SEC));
    assert_eq!(started(&out), [pair("small", "r2")]);
}

#[test]
fn filler_that_would_leave_the_head_only_the_reserve_waits() {
    let out = reserve_case(entry_with("mid", "agent", 2, req(2048, 1, 3600)));
    assert!(out.assignments.is_empty());
    assert!(matches!(
        wait_of(&out, "mid"),
        Some(Wait::HeadReserved { .. })
    ));
}

#[test]
fn filler_using_its_own_reserve_does_not_delay_the_head() {
    let out = reserve_case(entry_with("ci", "ci", 2, req(2048, 1, 3600)));
    assert_eq!(started(&out), [pair("ci", "r2")]);
}

#[test]
fn runner_accept_rule_shapes_the_reservation_and_the_filler_choice() {
    let mut heavy = runner("heavy");
    heavy.caps.accept.min_mem_mb = Some(4096);
    let runners = [heavy, runner("light")];
    let run = [running(
        entry_with("x", "agent", 0, req(1024, 1, 600)),
        "light",
        0,
    )];
    let queue = [
        entry_with("head", "agent", 1, req(1024, 1, 600)),
        entry_with("small-long", "agent", 2, req(1024, 1, 3600)),
        entry_with("big-long", "agent", 3, req(4096, 1, 3600)),
    ];
    let out = plan(&policy(16384, 8), &queue, &runners, &run, 100 * SEC);
    assert_eq!(wait_of(&out, "head"), Some(&Wait::RunnerBusy));
    assert_eq!(out.reservation.as_ref().unwrap().at, Timestamp(600 * SEC));
    assert_eq!(wait_of(&out, "small-long"), Some(&Wait::RunnerBusy));
    assert_eq!(started(&out), [pair("big-long", "heavy")]);
}
