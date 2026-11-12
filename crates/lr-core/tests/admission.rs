mod common;

use common::{entry, entry_with, policy, req, runner, runner_at, running, site};
use lr_core::{
    pick_next, Admission, ForeignLoadMode, LoadResource, MachineLoad, Plan, QueueEntry, QueueState,
    RulePolicy, RunnerView, RunningJob, ServeLog, Timestamp, Wait,
};

fn waiting(mem_headroom_mb: u64) -> RulePolicy {
    let mut p = policy(16384, 8);
    p.admission = Admission {
        foreign_load: ForeignLoadMode::Wait,
        mem_headroom_mb,
    };
    p
}

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

fn loaded(mut r: RunnerView, free_mem_mb: u64, cpu_busy_percent: u8) -> RunnerView {
    r.load = Some(MachineLoad {
        free_mem_mb,
        cpu_busy_percent,
        cpu_cores: 8,
    });
    r
}

fn job(mem_mb: u64, cpu: u32) -> QueueEntry {
    entry_with("j", "agent", 0, req(mem_mb, cpu, 600))
}

#[test]
fn the_texts_name_the_foreign_load() {
    let mem = Wait::ForeignLoad {
        resource: LoadResource::Memory,
        needed: 2000,
        level: 800,
    };
    assert_eq!(
        mem.to_string(),
        "занято чужими процессами: свободно 800 МБ, нужно 2000"
    );
    let cpu = Wait::ForeignLoad {
        resource: LoadResource::Cpu,
        needed: 4,
        level: 95,
    };
    assert_eq!(
        cpu.to_string(),
        "занято чужими процессами: CPU 95%, нужно 4 ядер"
    );
}

#[test]
fn memory_taken_by_others_keeps_the_order_queued() {
    let r = loaded(runner("r"), 1500, 0);
    let out = plan(&waiting(256), &[job(2048, 1)], &[r], &[]);
    assert!(out.assignments.is_empty());
    assert_eq!(
        out.waiting[0].1,
        Wait::ForeignLoad {
            resource: LoadResource::Memory,
            needed: 2304,
            level: 1500,
        }
    );
}

#[test]
fn enough_free_memory_and_cpu_starts_the_order() {
    let r = loaded(runner("r"), 4096, 50);
    let out = plan(&waiting(256), &[job(2048, 4)], &[r], &[]);
    assert_eq!(out.assignments.len(), 1);
}

#[test]
fn cpu_taken_by_others_keeps_the_order_queued() {
    let r = loaded(runner("r"), 8192, 95);
    let out = plan(&waiting(256), &[job(1024, 4)], &[r], &[]);
    assert_eq!(
        out.waiting[0].1,
        Wait::ForeignLoad {
            resource: LoadResource::Cpu,
            needed: 4,
            level: 95,
        }
    );
}

#[test]
fn the_default_admission_ignores_the_load() {
    let r = loaded(runner("r"), 1, 100);
    let out = plan(&policy(16384, 8), &[job(2048, 4)], &[r], &[]);
    assert_eq!(out.assignments.len(), 1);
}

#[test]
fn a_runner_without_a_load_is_admitted_as_before() {
    let out = plan(&waiting(256), &[job(2048, 4)], &[runner("r")], &[]);
    assert_eq!(out.assignments.len(), 1);
}

#[test]
fn another_runner_with_room_takes_the_order() {
    let busy = loaded(runner_at("busy", "192.168.1.20"), 100, 100);
    let free = loaded(runner_at("free", "192.168.1.21"), 8192, 10);
    let out = plan(&waiting(0), &[job(2048, 2)], &[busy, free], &[]);
    assert_eq!(out.assignments[0].runner, "free");
}

#[test]
fn a_sibling_job_on_the_same_machine_is_not_counted_as_foreign() {
    let a = loaded(runner_at("a", "192.168.1.20"), 8192, 50);
    let b = loaded(runner_at("b", "192.168.1.20"), 8192, 50);
    let sibling = running(entry("s", "agent", 0), "a", 0);
    let mut own = entry("s", "agent", 0);
    own.order.requirements = req(1024, 4, 600);
    let sibling = RunningJob {
        entry: own,
        ..sibling
    };
    let out = plan(
        &waiting(0),
        &[entry_with("n", "agent", 1, req(1024, 4, 600))],
        &[a, b],
        &[sibling],
    );
    assert_eq!(out.assignments.len(), 1, "{:?}", out.waiting);
}

#[test]
fn a_sibling_job_does_not_hide_foreign_load_above_its_reservation() {
    let a = loaded(runner_at("a", "192.168.1.20"), 8192, 100);
    let b = loaded(runner_at("b", "192.168.1.20"), 8192, 100);
    let mut own = entry("s", "agent", 0);
    own.order.requirements = req(1024, 2, 600);
    let sibling = running(own, "a", 0);
    let out = plan(
        &waiting(0),
        &[entry_with("n", "agent", 1, req(1024, 4, 600))],
        &[a, b],
        &[sibling],
    );
    assert!(out.assignments.is_empty());
    assert!(matches!(
        out.waiting[0].1,
        Wait::ForeignLoad {
            resource: LoadResource::Cpu,
            ..
        }
    ));
}

#[test]
fn memory_of_a_job_assigned_in_the_same_pass_is_set_aside() {
    let a = loaded(runner_at("a", "192.168.1.20"), 3000, 0);
    let b = loaded(runner_at("b", "192.168.1.20"), 3000, 0);
    let first = entry_with("first", "agent", 0, req(2000, 1, 600));
    let second = entry_with("second", "agent", 1, req(2000, 1, 600));
    let out = plan(&waiting(0), &[first, second], &[a, b], &[]);
    assert_eq!(out.assignments.len(), 1);
    assert_eq!(out.waiting.len(), 1);
    assert!(matches!(out.waiting[0].1, Wait::ForeignLoad { .. }));
}

#[test]
fn a_foreign_load_wait_does_not_reserve_the_start_for_a_head() {
    let r = loaded(runner("r"), 100, 0);
    let out = plan(&waiting(0), &[job(2048, 1)], &[r], &[]);
    assert!(out.reservation.is_none());
}
