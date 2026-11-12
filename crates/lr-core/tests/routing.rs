mod common;

use common::{entry, entry_with, ipc_runner, policy, req, runner, runner_at, running, set, site};
use lr_core::{
    check_runners, pick_next, suitable, Endpoint, MachineRef, MismatchReason, Plan, QueueEntry,
    QueueState, Reject, RulePolicy, RunnerView, RunningJob, ServeLog, Timestamp, Wait, Zone,
};

fn plan(p: &RulePolicy, queue: &[QueueEntry], runners: &[RunnerView], run: &[RunningJob]) -> Plan {
    let served = ServeLog::default();
    let state = QueueState {
        queue,
        runners,
        running: run,
        served: &served,
    };
    pick_next(p, &site(), &state, Timestamp(1_000))
}

fn runner_of(p: &Plan, job: &str) -> Option<String> {
    p.assignments
        .iter()
        .find(|a| a.job.0 == job)
        .map(|a| a.runner.clone())
}

fn same_machine_entry(job: &str, user: &str) -> QueueEntry {
    let mut r = req(1024, 1, 600);
    r.platform = None;
    r.same_machine = Some(MachineRef(user.into()));
    let mut e = entry_with(job, "agent", 0, r);
    e.client.endpoint = Endpoint::Ipc {
        os_user: user.into(),
    };
    e
}

#[test]
fn same_machine_order_only_matches_that_installation() {
    let runners = [
        runner("lan-free"),
        ipc_runner("bob-runner", "bob"),
        ipc_runner("alex-runner", "alex"),
    ];
    let e = same_machine_entry("wt", "alex");
    let out = plan(&policy(8192, 8), std::slice::from_ref(&e), &runners, &[]);
    assert_eq!(runner_of(&out, "wt").as_deref(), Some("alex-runner"));
    let busy = running(entry("x", "agent", 0), "alex-runner", 0);
    let out = plan(
        &policy(8192, 8),
        std::slice::from_ref(&e),
        &runners,
        &[busy],
    );
    assert!(out.assignments.is_empty());
    assert_eq!(out.waiting[0].1, Wait::RunnerBusy);
    assert_eq!(Wait::RunnerBusy.to_string(), "раннер занят");
}

#[test]
fn same_machine_needs_local_ipc_not_an_equal_machine_ref() {
    let mut net = runner("net");
    net.caps.machine = Some(MachineRef("alex".into()));
    let e = same_machine_entry("wt", "alex");
    let err = suitable(&e, &net, &site()).unwrap_err();
    assert!(err.reasons.contains(&MismatchReason::NotSameMachine));
    assert_eq!(
        check_runners(&e, &[net], &site()),
        Err(Reject::NoRunnerOnMachine)
    );
}

#[test]
fn same_machine_rejected_when_installation_runners_absent_or_offline() {
    let e = same_machine_entry("wt", "alex");
    assert_eq!(
        check_runners(&e, &[runner("lan")], &site()),
        Err(Reject::NoRunnerOnMachine)
    );
    let mut off = ipc_runner("alex-runner", "alex");
    off.online = false;
    assert_eq!(
        check_runners(&e, &[off], &site()),
        Err(Reject::NoRunnerOnMachine)
    );
    assert!(Reject::NoRunnerOnMachine.to_string().contains("up"));
}

#[test]
fn same_machine_with_online_unsuitable_runner_rejected_with_numbers() {
    let mut e = same_machine_entry("wt", "alex");
    e.order.requirements.mem_mb = 32768;
    let err = check_runners(&e, &[ipc_runner("alex-runner", "alex")], &site()).unwrap_err();
    let Reject::NoMatchingRunner { runners } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(runners[0].0, "alex-runner");
    assert!(err.to_string().contains("нужно 32768 МБ, у раннера 16384"));
}

#[test]
fn same_machine_waits_when_its_runner_is_online_but_busy() {
    let e = same_machine_entry("wt", "alex");
    assert!(check_runners(&e, &[ipc_runner("alex-runner", "alex")], &site()).is_ok());
}

#[test]
fn order_no_runner_suits_is_rejected_with_numbers() {
    let mut r = req(1024, 1, 600);
    r.programs = vec![lr_core::Program {
        name: "adb".into(),
        check: None,
    }];
    let e = entry_with("x", "agent", 0, r);
    let mut off = runner("off");
    off.online = false;
    let err = check_runners(&e, &[runner("r1"), off], &site()).unwrap_err();
    assert!(err.to_string().contains("r1: adb не найден"));
    assert!(err.to_string().contains("off: adb не найден"));
}

#[test]
fn order_whose_suitable_runners_are_offline_waits() {
    let e = entry("x", "agent", 0);
    let mut off = runner("off");
    off.online = false;
    assert!(check_runners(&e, std::slice::from_ref(&off), &site()).is_ok());
    let out = plan(&policy(8192, 8), &[e], &[off], &[]);
    assert_eq!(out.waiting[0].1, Wait::RunnerOffline);
}

fn zoned_runners() -> Vec<RunnerView> {
    vec![
        runner_at("remote", "8.8.8.8"),
        runner_at("lan", "192.168.1.20"),
        RunnerView {
            caps: lr_core::Capabilities {
                machine: Some(MachineRef("alex".into())),
                ..common::caps()
            },
            ..ipc_runner("local", "alex")
        },
    ]
}

fn local_client(job: &str) -> QueueEntry {
    let mut e = entry(job, "agent", 0);
    e.client.endpoint = Endpoint::Ipc {
        os_user: "alex".into(),
    };
    e
}

#[test]
fn nearest_suitable_runner_is_preferred() {
    let out = plan(
        &policy(8192, 8),
        &[local_client("a")],
        &zoned_runners(),
        &[],
    );
    assert_eq!(runner_of(&out, "a").as_deref(), Some("local"));
    assert_eq!(out.assignments[0].zone, Zone::Local);
}

#[test]
fn zone_order_can_be_reversed() {
    let mut p = policy(8192, 8);
    p.runner_zone_order = vec![Zone::Remote, Zone::Lan, Zone::Local];
    let out = plan(&p, &[local_client("a")], &zoned_runners(), &[]);
    assert_eq!(runner_of(&out, "a").as_deref(), Some("remote"));
}

#[test]
fn busy_near_runner_does_not_block_a_free_far_one() {
    let busy = running(entry("x", "agent", 0), "local", 0);
    let out = plan(
        &policy(8192, 8),
        &[local_client("a")],
        &zoned_runners(),
        &[busy],
    );
    assert_eq!(runner_of(&out, "a").as_deref(), Some("lan"));
}

#[test]
fn unsuitable_near_runner_is_skipped_for_a_suitable_far_one() {
    let mut runners = zoned_runners();
    runners[2].caps.platforms = set(&["windows"]);
    runners[1].caps.platforms = set(&["windows"]);
    let out = plan(&policy(8192, 8), &[local_client("a")], &runners, &[]);
    assert_eq!(runner_of(&out, "a").as_deref(), Some("remote"));
}

#[test]
fn order_zone_limit_is_a_hard_boundary() {
    let mut e = local_client("a");
    e.order.requirements.max_zone = Some(Zone::Lan);
    let runners = zoned_runners();
    let busy = [
        running(entry("x", "agent", 0), "local", 0),
        running(entry("y", "agent", 0), "lan", 0),
    ];
    let mut p = policy(8192, 8);
    p.runner_zone_order = vec![Zone::Remote, Zone::Lan, Zone::Local];
    let out = plan(&p, std::slice::from_ref(&e), &runners, &busy);
    assert!(out.assignments.is_empty());
    assert_eq!(out.waiting[0].1, Wait::RunnerBusy);
    let err = suitable(&e, &runners[0], &site()).unwrap_err();
    assert!(err.reasons.contains(&MismatchReason::Zone {
        runner: Zone::Remote,
        allowed: Zone::Lan
    }));
}

#[test]
fn zone_is_pairwise_per_client_not_from_the_runner_report() {
    let mut office = runner_at("office", "10.1.0.7");
    office.caps.zone = Zone::Local;
    let mut e = entry("a", "agent", 0);
    e.order.requirements.max_zone = Some(Zone::Lan);
    assert_eq!(
        suitable(&e, &office, &site()).unwrap_err().reasons,
        [MismatchReason::Zone {
            runner: Zone::Remote,
            allowed: Zone::Lan
        }]
    );
    e.client.endpoint = Endpoint::Addr("10.1.3.3".parse().unwrap());
    assert_eq!(suitable(&e, &office, &site()), Ok(Zone::Lan));
}

#[test]
fn equal_zone_prefers_largest_capacity() {
    let small = runner_at("a-small", "192.168.1.21");
    let mut big = runner_at("z-big", "192.168.1.22");
    big.caps.mem_mb = 65536;
    let out = plan(
        &policy(8192, 8),
        &[entry("a", "agent", 0)],
        &[small, big],
        &[],
    );
    assert_eq!(runner_of(&out, "a").as_deref(), Some("z-big"));
}
