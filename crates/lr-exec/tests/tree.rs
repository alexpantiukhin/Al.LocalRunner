mod common;

use common::{
    clock, fresh_file, helper_command, job, pid_alive, real_sleep, step, Helper, Recorder, T0,
};
use lr_abstractions::{
    stop_signal, Clock, JobOutcome, JobSpec, ProcessRunner, Stop, StopHandle, DEFAULT_CANCEL_GRACE,
};
use lr_core::Timestamp;
use lr_core::{JobState, StepState};
use lr_exec::{cancel_line, ExecRunner, SOFT_STOP_NOT_DELIVERED};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

fn start(
    spec: JobSpec,
    clock: Arc<dyn Clock>,
) -> (JoinHandle<JobOutcome>, Arc<Recorder>, StopHandle) {
    let runner = ExecRunner::new(clock);
    let out = Recorder::new();
    let (handle, stop) = stop_signal();
    let o = out.clone();
    let task = tokio::spawn(async move { runner.run_job(spec, o.as_ref(), stop).await });
    (task, out, handle)
}

async fn finish(task: JoinHandle<JobOutcome>) -> JobOutcome {
    tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .expect("run_job did not return")
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_kills_tree_without_soft_signal() {
    let (manual, clock) = clock();
    let m1 = fresh_file("tree", "timeout-child.marker");
    let m2 = fresh_file("tree", "timeout-grandchild.marker");
    let cmd = helper_command(&Helper::new("tree").marker(&m1).marker2(&m2));
    let spec = job("t", 600, vec![step("s", &cmd, 30)]);
    let (task, out, _stop) = start(spec, clock);
    let child = out.pid_of("child").await;
    let grand = out.pid_of("grandchild").await;
    assert!(pid_alive(child) && pid_alive(grand));
    manual.advance(Duration::from_secs(30));
    let outcome = finish(task).await;
    assert!(!pid_alive(child), "child {child} survived the timeout");
    assert!(!pid_alive(grand), "grandchild {grand} survived the timeout");
    assert_eq!(outcome.state, JobState::Timeout);
    assert_eq!(outcome.steps[0].state, StepState::Timeout);
    assert_eq!(outcome.steps[0].exit_code, None);
    real_sleep(300).await;
    assert!(
        !m1.exists() && !m2.exists(),
        "a soft signal was sent on timeout"
    );
    assert!(!out.text().contains("got-soft"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_sends_soft_signal_then_kills_after_grace_10s() {
    let (manual, clock) = clock();
    let m1 = fresh_file("tree", "cancel-child.marker");
    let cmd = helper_command(&Helper::new("tree").marker(&m1));
    let spec = job(
        "c",
        600,
        vec![step("s", &cmd, 300), step("after", "echo never", 30)],
    );
    assert_eq!(spec.cancel_grace, Duration::from_secs(10));
    assert_eq!(DEFAULT_CANCEL_GRACE, Duration::from_secs(10));
    let (task, out, stop) = start(spec, clock);
    let child = out.pid_of("child").await;
    let grand = out.pid_of("grandchild").await;
    stop.stop_with_reason(Stop::Cancel, "bob at 12:00: stale");
    out.wait_for("child-got-soft").await;
    assert!(m1.exists());
    real_sleep(500).await;
    assert!(!task.is_finished(), "killed before the grace");
    assert!(pid_alive(child) && pid_alive(grand));
    manual.advance(Duration::from_millis(9_900));
    real_sleep(500).await;
    assert!(!task.is_finished(), "killed before the grace of 10 s");
    assert!(pid_alive(child) && pid_alive(grand));
    manual.advance(Duration::from_millis(100));
    let outcome = finish(task).await;
    assert!(!pid_alive(child), "child {child} survived the cancel");
    assert!(!pid_alive(grand), "grandchild {grand} survived the cancel");
    assert_eq!(outcome.state, JobState::Cancelled);
    assert_eq!(
        outcome.steps.iter().map(|s| s.state).collect::<Vec<_>>(),
        vec![StepState::Cancelled, StepState::Skipped]
    );
    assert!(!out.text().contains("never"));
    let trailer = cancel_line("s", Timestamp(T0.0 + 10_000), Some("bob at 12:00: stale"));
    assert!(out.text().ends_with(&trailer), "{}", out.text());
    assert!(!out.text().contains(SOFT_STOP_NOT_DELIVERED));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_of_a_step_that_obeys_ends_without_waiting_for_grace() {
    let (_manual, clock) = clock();
    let cmd = helper_command(&Helper::new("obey"));
    let spec = job("o", 600, vec![step("s", &cmd, 300)]);
    let (task, out, stop) = start(spec, clock);
    let child = out.pid_of("child").await;
    stop.stop(Stop::Cancel);
    let outcome = finish(task).await;
    assert_eq!(outcome.state, JobState::Cancelled);
    assert_eq!(outcome.steps[0].state, StepState::Cancelled);
    assert!(out.text().contains("child-bye"));
    assert!(!pid_alive(child));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_stops_the_tree_at_once_without_soft_signal() {
    let (_manual, clock) = clock();
    let m1 = fresh_file("tree", "kill-child.marker");
    let cmd = helper_command(&Helper::new("tree").marker(&m1));
    let spec = job("k", 600, vec![step("s", &cmd, 300)]);
    let (task, out, stop) = start(spec, clock);
    let child = out.pid_of("child").await;
    let grand = out.pid_of("grandchild").await;
    stop.stop(Stop::Kill);
    let outcome = finish(task).await;
    assert!(!pid_alive(child) && !pid_alive(grand));
    assert_eq!(outcome.state, JobState::Cancelled);
    assert_eq!(outcome.steps[0].state, StepState::Cancelled);
    real_sleep(300).await;
    assert!(!m1.exists());
    assert!(
        out.text().ends_with(&cancel_line("s", T0, None)),
        "{}",
        out.text()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_during_grace_kills_at_once() {
    let (_manual, clock) = clock();
    let cmd = helper_command(&Helper::new("ignore"));
    let spec = job("kg", 600, vec![step("s", &cmd, 300)]);
    let (task, out, stop) = start(spec, clock);
    let child = out.pid_of("child").await;
    stop.stop(Stop::Cancel);
    out.wait_for("child-got-soft").await;
    stop.stop(Stop::Kill);
    let outcome = finish(task).await;
    assert!(!pid_alive(child));
    assert_eq!(outcome.state, JobState::Cancelled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn leftover_descendants_in_the_group_die_when_the_step_exits() {
    let (_manual, clock) = clock();
    let cmd = helper_command(&Helper::new("spawn-exit").in_group());
    let spec = job("l", 600, vec![step("s", &cmd, 300)]);
    let (task, out, _stop) = start(spec, clock);
    let grand = out.pid_of("grandchild").await;
    let outcome = finish(task).await;
    assert!(!pid_alive(grand), "leftover {grand} survived the step");
    assert_eq!(outcome.state, JobState::Passed);
    assert_eq!(outcome.steps[0].exit_code, Some(0));
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn detached_leftover_descendant_dies_with_the_job_object() {
    let (_manual, clock) = clock();
    let cmd = helper_command(&Helper::new("spawn-exit"));
    let spec = job("d", 600, vec![step("s", &cmd, 300)]);
    let (task, out, _stop) = start(spec, clock);
    let grand = out.pid_of("grandchild").await;
    let outcome = finish(task).await;
    assert!(!pid_alive(grand), "detached leftover {grand} survived");
    assert_eq!(outcome.state, JobState::Passed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_a_running_job_kills_its_tree() {
    let (_manual, clock) = clock();
    let cmd = helper_command(&Helper::new("tree"));
    let spec = job("drop", 600, vec![step("s", &cmd, 300)]);
    let (task, out, _stop) = start(spec, clock);
    let child = out.pid_of("child").await;
    let grand = out.pid_of("grandchild").await;
    task.abort();
    let _ = task.await;
    let mut alive = true;
    for _ in 0..50 {
        alive = pid_alive(child) || pid_alive(grand);
        if !alive {
            break;
        }
        real_sleep(100).await;
    }
    assert!(!alive, "tree survived a dropped run_job");
}

#[cfg(target_os = "linux")]
fn systemd_scope_works() -> bool {
    std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").is_file()
        && std::process::Command::new("systemd-run")
            .args(["--user", "--scope", "--quiet", "--collect", "--"])
            .args(["/bin/sh", "-c", ":"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

#[cfg(target_os = "linux")]
#[test]
fn linux_containment_is_a_cgroup_scope_exactly_when_systemd_run_works() {
    let expected = if systemd_scope_works() {
        lr_exec::Containment::CgroupScope
    } else {
        lr_exec::Containment::ProcessGroup
    };
    assert_eq!(lr_exec::containment(), expected);
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linux_detached_leftover_descendant_dies_with_the_cgroup_scope() {
    if lr_exec::containment() != lr_exec::Containment::CgroupScope {
        eprintln!("no systemd user scope here: the process-group fallback is covered elsewhere");
        return;
    }
    let (_manual, clock) = clock();
    let cmd = helper_command(&Helper::new("spawn-exit"));
    let spec = job("cg", 600, vec![step("s", &cmd, 300)]);
    let (task, out, _stop) = start(spec, clock);
    let grand = out.pid_of("grandchild").await;
    let outcome = finish(task).await;
    let mut alive = true;
    for _ in 0..50 {
        alive = pid_alive(grand);
        if !alive {
            break;
        }
        real_sleep(100).await;
    }
    assert!(!alive, "detached leftover {grand} escaped the scope");
    assert_eq!(outcome.state, JobState::Passed);
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linux_job_environment_gets_no_bus_variables_from_the_scope_wrapper() {
    let (_manual, clock) = clock();
    let spec = job("env", 600, vec![step("s", "env", 30)]);
    let (task, out, _stop) = start(spec, clock);
    let outcome = finish(task).await;
    assert_eq!(outcome.state, JobState::Passed);
    let text = out.text();
    assert!(!text.contains("DBUS_SESSION_BUS_ADDRESS="), "{text}");
    assert!(!text.contains("XDG_RUNTIME_DIR="), "{text}");
    assert!(text.contains("PATH="), "{text}");
}
