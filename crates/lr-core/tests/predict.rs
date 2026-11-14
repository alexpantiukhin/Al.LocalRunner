mod common;

use common::{
    entry, entry_with, job, pair, plan, policy, req, runner, running, set, site, started, wait_of,
    MIN, SEC,
};
use lr_core::{
    earliest_start, Condition, Reservation, Reserve, Rule, RulePolicy, RunnerView, RunningJob,
    Timestamp, Wait,
};

#[test]
fn free_order_starts_on_a_free_runner() {
    let p = plan(
        &policy(8192, 8),
        &[entry("a", "agent", 0)],
        &[runner("r1")],
        &[],
        0,
    );
    assert_eq!(started(&p), [pair("a", "r1")]);
    assert!(p.waiting.is_empty());
    assert_eq!(p.reservation, None);
}

#[test]
fn one_job_per_runner_at_a_time() {
    let queue = [entry("a", "agent", 0), entry("b", "agent", 1)];
    let p = plan(&policy(8192, 8), &queue, &[runner("r1")], &[], 0);
    assert_eq!(started(&p), [pair("a", "r1")]);
    assert_eq!(wait_of(&p, "b"), Some(&Wait::RunnerBusy));
}

#[test]
fn high_priority_never_preempts_running() {
    let low = running(entry_with("low", "agent", 0, req(1024, 1, 600)), "r1", 0);
    let queue = [entry("ci", "ci", 10 * SEC)];
    let p = plan(&policy(8192, 8), &queue, &[runner("r1")], &[low], 20 * SEC);
    assert!(p.assignments.is_empty());
    assert_eq!(wait_of(&p, "ci"), Some(&Wait::RunnerBusy));
    assert_eq!(
        p.reservation,
        Some(Reservation {
            job: job("ci"),
            at: Timestamp(600 * SEC)
        })
    );
}

fn memory_head_setup() -> (RulePolicy, Vec<RunnerView>, Vec<RunningJob>) {
    let p = policy(8192, 8);
    let runners = vec![runner("r1"), runner("r2"), runner("r3")];
    let a = running(entry_with("a", "agent", 0, req(4096, 1, 600)), "r1", 0);
    (p, runners, vec![a])
}

#[test]
fn short_job_fills_hole_before_head_start() {
    let (p, runners, run) = memory_head_setup();
    let queue = [
        entry_with("head", "agent", 1, req(6144, 1, 600)),
        entry_with("short", "agent", 2, req(4096, 1, 300)),
    ];
    let out = plan(&p, &queue, &runners, &run, 100 * SEC);
    assert_eq!(started(&out), [pair("short", "r2")]);
    assert_eq!(
        wait_of(&out, "head"),
        Some(&Wait::Memory {
            needed: 6144,
            free: 4096
        })
    );
    assert_eq!(out.reservation.as_ref().unwrap().at, Timestamp(600 * SEC));
}

#[test]
fn hole_filler_may_end_exactly_at_the_reservation() {
    let (p, runners, run) = memory_head_setup();
    let queue = [
        entry_with("head", "agent", 1, req(6144, 1, 600)),
        entry_with("edge", "agent", 2, req(4096, 1, 500)),
    ];
    let out = plan(&p, &queue, &runners, &run, 100 * SEC);
    assert_eq!(started(&out), [pair("edge", "r2")]);
}

#[test]
fn long_job_does_not_delay_head() {
    let (p, runners, run) = memory_head_setup();
    let queue = [
        entry_with("head", "agent", 1, req(6144, 1, 600)),
        entry_with("long", "agent", 2, req(4096, 1, 3600)),
    ];
    let out = plan(&p, &queue, &runners, &run, 100 * SEC);
    assert!(out.assignments.is_empty());
    assert_eq!(
        wait_of(&out, "long"),
        Some(&Wait::HeadReserved {
            head: job("head"),
            at: Timestamp(600 * SEC)
        })
    );
}

#[test]
fn long_job_on_leftover_resources_bypasses_head() {
    let (p, runners, run) = memory_head_setup();
    let queue = [
        entry_with("head", "agent", 1, req(6144, 1, 600)),
        entry_with("small", "agent", 2, req(1024, 1, 3600)),
    ];
    let out = plan(&p, &queue, &runners, &run, 100 * SEC);
    assert_eq!(started(&out), [pair("small", "r2")]);
}

#[test]
fn long_job_does_not_take_the_runner_head_needs() {
    let p = policy(8192, 8);
    let mut ios = runner("ios");
    ios.caps.platforms = set(&["linux", "ios"]);
    let runners = vec![ios, runner("r2")];
    let a = running(entry_with("a", "agent", 0, req(4096, 1, 600)), "r2", 0);
    let mut head_req = req(6144, 1, 600);
    head_req.platform = Some("ios".into());
    let queue = [
        entry_with("head", "agent", 1, head_req),
        entry_with("long", "agent", 2, req(1024, 1, 3600)),
        entry_with("short", "agent", 3, req(1024, 1, 60)),
    ];
    let out = plan(&p, &queue, &runners, &[a], 100 * SEC);
    assert_eq!(started(&out), [pair("short", "ios")]);
    assert!(matches!(
        wait_of(&out, "long"),
        Some(Wait::HeadReserved { .. })
    ));
}

#[test]
fn several_hole_fillers_together_do_not_delay_head() {
    let (p, runners, run) = memory_head_setup();
    let queue = [
        entry_with("head", "agent", 1, req(6144, 1, 600)),
        entry_with("s1", "agent", 2, req(2048, 1, 3600)),
        entry_with("s2", "agent", 3, req(2048, 1, 3600)),
    ];
    let out = plan(&p, &queue, &runners, &run, 100 * SEC);
    assert_eq!(started(&out), [pair("s1", "r2")]);
    assert!(matches!(
        wait_of(&out, "s2"),
        Some(Wait::HeadReserved { .. })
    ));
}

#[test]
fn reservation_recomputed_when_running_job_ends_early() {
    let (p, runners, _) = memory_head_setup();
    let (j1, j2) = two_jobs();
    let queue = [entry_with("head", "agent", 1, req(7000, 1, 600))];
    let before = plan(&p, &queue, &runners, &[j1.clone(), j2], 100 * SEC);
    assert_eq!(
        before.reservation.as_ref().unwrap().at,
        Timestamp(600 * SEC)
    );
    let after = plan(&p, &queue, &runners, std::slice::from_ref(&j1), 200 * SEC);
    assert!(after.assignments.is_empty());
    assert_eq!(after.reservation.as_ref().unwrap().at, Timestamp(300 * SEC));
    let done = plan(&p, &queue, &runners, &[], 250 * SEC);
    assert_eq!(started(&done), [pair("head", "r1")]);
    assert_eq!(done.reservation, None);
}

fn two_jobs() -> (RunningJob, RunningJob) {
    (
        running(entry_with("j1", "agent", 0, req(3072, 1, 300)), "r1", 0),
        running(entry_with("j2", "agent", 0, req(3072, 1, 600)), "r2", 0),
    )
}

#[test]
fn head_waits_for_every_job_whose_resources_it_needs() {
    let (p, mut runners, _) = memory_head_setup();
    runners.push(runner("r4"));
    let (j1, j2) = two_jobs();
    let run = [j1, j2];
    let head = entry_with("head", "agent", 1, req(7000, 1, 600));
    assert_eq!(
        earliest_start(&p, &site(), &head, &runners, &run, Timestamp(100 * SEC)),
        Some(Timestamp(600 * SEC))
    );
    let queue = [
        head,
        entry_with("between", "agent", 2, req(512, 1, 350)),
        entry_with("after", "agent", 3, req(1536, 1, 600)),
    ];
    let out = plan(&p, &queue, &runners, &run, 100 * SEC);
    assert_eq!(out.reservation.as_ref().unwrap().at, Timestamp(600 * SEC));
    assert_eq!(started(&out), [pair("between", "r3")]);
    assert!(matches!(
        wait_of(&out, "after"),
        Some(Wait::HeadReserved { .. })
    ));
}

#[test]
fn hole_filler_ending_one_ms_after_the_reservation_waits() {
    let (p, runners, run) = memory_head_setup();
    let queue = [
        entry_with("head", "agent", 1, req(6144, 1, 600)),
        entry_with("late", "agent", 2, req(4096, 1, 500)),
    ];
    let out = plan(&p, &queue, &runners, &run, 100 * SEC + 1);
    assert!(out.assignments.is_empty());
    assert_eq!(
        wait_of(&out, "late"),
        Some(&Wait::HeadReserved {
            head: job("head"),
            at: Timestamp(600 * SEC)
        })
    );
}

#[test]
fn filler_wanting_a_tag_the_head_holds_exclusively_waits() {
    let (p, runners, run) = memory_head_setup();
    let mut head = req(6144, 1, 600);
    head.exclusive = set(&["emu"]);
    let mut long = req(1024, 1, 3600);
    long.tags = set(&["emu"]);
    let mut short = req(1024, 1, 60);
    short.tags = set(&["emu"]);
    let mut tagged = runners.clone();
    for r in &mut tagged {
        r.caps.tags = set(&["emu"]);
    }
    let queue = [
        entry_with("head", "agent", 1, head),
        entry_with("long", "agent", 2, long),
    ];
    let out = plan(&p, &queue, &tagged, &run, 100 * SEC);
    assert!(out.assignments.is_empty());
    assert!(matches!(
        wait_of(&out, "long"),
        Some(Wait::HeadReserved { .. })
    ));
    let queue = [queue[0].clone(), entry_with("short", "agent", 2, short)];
    let out = plan(&p, &queue, &tagged, &run, 100 * SEC);
    assert_eq!(started(&out), [pair("short", "r2")]);
}

#[test]
fn overdue_running_job_makes_head_reserve_now() {
    let (p, runners, run) = memory_head_setup();
    let queue = [
        entry_with("head", "agent", 1, req(6144, 1, 600)),
        entry_with("short", "agent", 2, req(4096, 1, 60)),
    ];
    let out = plan(&p, &queue, &runners, &run, 700 * SEC);
    assert_eq!(out.reservation.as_ref().unwrap().at, Timestamp(700 * SEC));
    assert!(out.assignments.is_empty());
}

#[test]
fn head_with_offline_runners_does_not_hold_the_pool() {
    let mut ios = runner("ios");
    ios.caps.platforms = set(&["ios"]);
    ios.online = false;
    let mut head_req = req(1024, 1, 600);
    head_req.platform = Some("ios".into());
    let queue = [
        entry_with("head", "agent", 1, head_req),
        entry_with("next", "agent", 2, req(1024, 1, 3600)),
    ];
    let out = plan(&policy(8192, 8), &queue, &[ios, runner("r1")], &[], 0);
    assert_eq!(started(&out), [pair("next", "r1")]);
    assert_eq!(wait_of(&out, "head"), Some(&Wait::RunnerOffline));
    assert_eq!(out.reservation, None);
}

#[test]
fn only_the_first_blocked_order_reserves() {
    let (p, runners, run) = memory_head_setup();
    let queue = [
        entry_with("head", "agent", 1, req(6144, 1, 600)),
        entry_with("second", "agent", 2, req(8192, 1, 600)),
        entry_with("short", "agent", 3, req(2048, 1, 60)),
    ];
    let out = plan(&p, &queue, &runners, &run, 100 * SEC);
    assert_eq!(out.reservation.as_ref().unwrap().job, job("head"));
    assert_eq!(started(&out), [pair("short", "r2")]);
}

#[test]
fn exclusive_tag_holder_is_named_while_waiting() {
    let mut held = req(1024, 1, 600);
    held.tags = set(&["emu"]);
    held.exclusive = set(&["emu"]);
    let a = running(entry_with("a", "agent", 0, held.clone()), "r1", 0);
    let mut p = policy(8192, 8);
    p.pool.exclusive_tags = set(&["emu"]);
    let mut caps_runner = runner("r2");
    caps_runner.caps.tags = set(&["emu"]);
    let mut r1 = runner("r1");
    r1.caps.tags = set(&["emu"]);
    let mut want = req(1024, 1, 60);
    want.tags = set(&["emu"]);
    let queue = [entry_with("b", "agent", 1, want)];
    let out = plan(&p, &queue, &[r1, caps_runner], &[a], 10 * SEC);
    assert_eq!(
        wait_of(&out, "b"),
        Some(&Wait::Exclusive {
            tag: "emu".into(),
            holder: job("a")
        })
    );
    assert_eq!(out.reservation.as_ref().unwrap().at, Timestamp(600 * SEC));
}

#[test]
fn reserve_rule_holds_back_other_orders_in_planning() {
    let mut p = policy(8192, 4);
    p.rules.push(Rule {
        name: Some("ci-reserve".into()),
        when: Condition {
            client_label: Some("ci".into()),
            ..Condition::default()
        },
        reserve: Some(Reserve { cpu: 2, mem_mb: 0 }),
        ..Rule::default()
    });
    let queue = [entry_with("a", "agent", 1, req(1024, 3, 60))];
    let out = plan(&p, &queue, &[runner("r1")], &[], 0);
    assert!(out.assignments.is_empty());
    assert!(
        matches!(wait_of(&out, "a"), Some(Wait::Reserved { rule, .. }) if rule == "ci-reserve")
    );
}

#[test]
fn runner_accept_reserves_it_for_heavy_orders() {
    let mut heavy = runner("heavy");
    heavy.caps.accept.min_mem_mb = Some(8192);
    let queue = [entry_with("light", "agent", 1, req(1024, 1, 60))];
    let out = plan(&policy(16384, 8), &queue, &[heavy], &[], 0);
    assert_eq!(wait_of(&out, "light"), Some(&Wait::NoRunner));
}

#[test]
fn earliest_start_is_the_end_of_the_blocking_job() {
    let (p, runners, run) = memory_head_setup();
    let e = entry_with("head", "agent", 1, req(6144, 1, 600));
    assert_eq!(
        earliest_start(&p, &site(), &e, &runners, &run, Timestamp(100 * SEC)),
        Some(Timestamp(600 * SEC))
    );
    let small = entry_with("small", "agent", 1, req(1024, 1, 600));
    assert_eq!(
        earliest_start(&p, &site(), &small, &runners, &run, Timestamp(100 * SEC)),
        Some(Timestamp(100 * SEC))
    );
}

#[test]
fn waiting_longer_does_not_make_an_order_the_head() {
    let p = policy(8192, 8);
    let busy = running(entry_with("x", "agent", 0, req(1024, 1, 3600)), "r1", 0);
    let queue = [entry("ci", "ci", 20 * MIN), entry("old", "agent", 0)];
    let out = plan(&p, &queue, &[runner("r1")], &[busy], 31 * MIN);
    assert_eq!(out.reservation.unwrap().job, job("ci"));
}
