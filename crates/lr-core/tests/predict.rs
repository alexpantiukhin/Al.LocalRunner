mod common;

use common::{entry, entry_with, job, policy, req, runner, running, set, site, MIN};
use lr_core::{
    earliest_start, pick_next, Condition, Plan, QueueEntry, Reservation, Reserve, Rule, RulePolicy,
    RunnerView, RunningJob, ServeLog, Timestamp, Wait,
};

const SEC: u64 = 1_000;

fn plan(
    p: &RulePolicy,
    queue: &[QueueEntry],
    runners: &[RunnerView],
    run: &[RunningJob],
    now: u64,
) -> Plan {
    let served = ServeLog::default();
    let state = lr_core::QueueState {
        queue,
        runners,
        running: run,
        served: &served,
    };
    pick_next(p, &site(), &state, Timestamp(now))
}

fn started(p: &Plan) -> Vec<(String, String)> {
    p.assignments
        .iter()
        .map(|a| (a.job.0.clone(), a.runner.clone()))
        .collect()
}

fn wait_of<'a>(p: &'a Plan, id: &str) -> Option<&'a Wait> {
    p.waiting.iter().find(|(j, _)| j.0 == id).map(|(_, w)| w)
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

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
    assert_eq!(out.reservation.unwrap().at, Timestamp(600 * SEC));
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
    let queue = [entry_with("head", "agent", 1, req(6144, 1, 600))];
    let out = plan(&p, &queue, &runners, &[], 200 * SEC);
    assert_eq!(started(&out), [pair("head", "r1")]);
    assert_eq!(out.reservation, None);
}

#[test]
fn overdue_running_job_makes_head_reserve_now() {
    let (p, runners, run) = memory_head_setup();
    let queue = [
        entry_with("head", "agent", 1, req(6144, 1, 600)),
        entry_with("short", "agent", 2, req(4096, 1, 60)),
    ];
    let out = plan(&p, &queue, &runners, &run, 700 * SEC);
    assert_eq!(out.reservation.unwrap().at, Timestamp(700 * SEC));
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
    assert_eq!(out.reservation.unwrap().at, Timestamp(600 * SEC));
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
fn aged_order_becomes_head_and_reserves() {
    let p = policy(8192, 8);
    let busy = running(entry_with("x", "agent", 0, req(1024, 1, 3600)), "r1", 0);
    let queue = [entry("ci", "ci", 20 * MIN), entry("old", "agent", 0)];
    let out = plan(&p, &queue, &[runner("r1")], &[busy], 31 * MIN);
    assert_eq!(out.reservation.unwrap().job, job("old"));
}
