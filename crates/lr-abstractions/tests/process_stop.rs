mod common;

use common::{clock, job, settle, step, T0};
use lr_abstractions::fakes::{
    FakeProcessInspector, FakeProcessRunner, JobScript, RecordingOutput, StepScript,
};
use lr_abstractions::{
    stop_signal, Liveness, ProcessInspector, ProcessRef, ProcessRunner, SpawnSpec, Stop,
};
use lr_core::{JobState, StepState};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn two_step_runner(
    clock: Arc<dyn lr_abstractions::Clock>,
    first: StepScript,
) -> (Arc<FakeProcessRunner>, lr_abstractions::JobSpec) {
    let runner = Arc::new(FakeProcessRunner::new(clock));
    let spec = job("j", 1000, vec![step("a", 500), step("b", 500)]);
    runner.script(spec.job.clone(), JobScript::new(vec![first]));
    (runner, spec)
}

#[tokio::test]
async fn cancel_obeyed_ends_step_cancelled_and_skips_rest() {
    let (manual, clock) = clock();
    let (runner, spec) = two_step_runner(clock, StepScript::new(secs(100), 0));
    let (handle, stop) = stop_signal();
    let out = Arc::new(RecordingOutput::new());
    let o = out.clone();
    let task = tokio::spawn(async move { runner.run_job(spec, o.as_ref(), stop).await });
    manual.wait_for_sleepers(1).await;
    manual.advance(secs(2));
    handle.stop(Stop::Cancel);
    let outcome = task.await.unwrap();
    assert_eq!(outcome.state, JobState::Cancelled);
    assert_eq!(outcome.steps[0].state, StepState::Cancelled);
    assert_eq!(outcome.steps[0].finished_at, Some(T0.plus_secs(2)));
    assert_eq!(outcome.steps[1].state, StepState::Skipped);
}

#[tokio::test]
async fn cancel_ignored_kills_tree_after_grace() {
    let (manual, clock) = clock();
    let (runner, spec) = two_step_runner(clock, StepScript::new(secs(100), 0).ignores_soft_stop());
    let (handle, stop) = stop_signal();
    let out = Arc::new(RecordingOutput::new());
    let o = out.clone();
    let task = tokio::spawn(async move { runner.run_job(spec, o.as_ref(), stop).await });
    manual.wait_for_sleepers(1).await;
    handle.stop(Stop::Cancel);
    settle().await;
    manual.advance(secs(9));
    settle().await;
    assert!(!task.is_finished(), "killed before the 10 s grace");
    manual.advance(secs(1));
    let outcome = task.await.unwrap();
    assert_eq!(outcome.state, JobState::Cancelled);
    assert_eq!(outcome.steps[0].finished_at, Some(T0.plus_secs(10)));
}

#[tokio::test]
async fn kill_during_grace_stops_at_once() {
    let (manual, clock) = clock();
    let (runner, spec) = two_step_runner(clock, StepScript::new(secs(100), 0).ignores_soft_stop());
    let (handle, stop) = stop_signal();
    let out = Arc::new(RecordingOutput::new());
    let o = out.clone();
    let task = tokio::spawn(async move { runner.run_job(spec, o.as_ref(), stop).await });
    manual.wait_for_sleepers(1).await;
    handle.stop(Stop::Cancel);
    settle().await;
    manual.advance(secs(3));
    settle().await;
    handle.stop(Stop::Kill);
    let outcome = task.await.unwrap();
    assert_eq!(outcome.state, JobState::Cancelled);
    assert_eq!(outcome.steps[0].finished_at, Some(T0.plus_secs(3)));
}

#[tokio::test]
async fn kill_ignores_grace_even_for_stubborn_process() {
    let (manual, clock) = clock();
    let (runner, spec) = two_step_runner(clock, StepScript::new(secs(100), 0).ignores_soft_stop());
    let (handle, stop) = stop_signal();
    let out = Arc::new(RecordingOutput::new());
    let o = out.clone();
    let task = tokio::spawn(async move { runner.run_job(spec, o.as_ref(), stop).await });
    manual.wait_for_sleepers(1).await;
    handle.stop(Stop::Kill);
    let outcome = task.await.unwrap();
    assert_eq!(outcome.steps[0].state, StepState::Cancelled);
    assert_eq!(outcome.steps[0].finished_at, Some(T0));
}

#[tokio::test]
async fn stop_before_start_skips_every_step() {
    let (_manual, clock) = clock();
    let (runner, spec) = two_step_runner(clock, StepScript::new(secs(100), 0));
    let (handle, stop) = stop_signal();
    handle.stop(Stop::Cancel);
    let out = RecordingOutput::new();
    let outcome = runner.run_job(spec, &out, stop).await;
    assert_eq!(outcome.state, JobState::Cancelled);
    assert!(outcome.steps.iter().all(|s| s.state == StepState::Skipped));
    assert!(out.events().is_empty());
}

#[test]
fn stop_never_downgrades_kill_to_cancel() {
    let (handle, stop) = stop_signal();
    assert_eq!(stop.current(), None);
    handle.stop(Stop::Kill);
    handle.stop(Stop::Cancel);
    assert_eq!(stop.current(), Some(Stop::Kill));
}

#[tokio::test]
async fn spawned_service_is_alive_until_its_tree_is_killed() {
    let (_, clock) = clock();
    let runner = FakeProcessRunner::new(clock);
    let inspector = runner.processes();
    let proc = runner
        .spawn(SpawnSpec::new("daemon --start", PathBuf::from(".")))
        .await
        .unwrap();
    assert_eq!(
        inspector.liveness(proc.pid, proc.start.unwrap()),
        Liveness::Alive
    );
    runner.kill_tree(&proc).await.unwrap();
    assert_eq!(
        inspector.liveness(proc.pid, proc.start.unwrap()),
        Liveness::Dead
    );
    assert_eq!(runner.kills(), vec![proc.clone()]);
    runner.kill_tree(&proc).await.unwrap();
}

#[test]
fn reused_pid_with_other_start_time_counts_as_dead() {
    let inspector = FakeProcessInspector::new();
    inspector.add(ProcessRef {
        pid: 100,
        start: Some(5),
    });
    assert_eq!(inspector.liveness(100, 5), Liveness::Alive);
    assert_eq!(inspector.liveness(100, 6), Liveness::Dead);
    assert_eq!(inspector.liveness(101, 5), Liveness::Dead);
}

#[test]
fn unknown_start_time_or_pid_zero_is_never_reported_dead() {
    let inspector = FakeProcessInspector::new();
    inspector.add(ProcessRef {
        pid: 7,
        start: None,
    });
    assert_eq!(inspector.liveness(7, 1), Liveness::Unknown);
    assert_eq!(inspector.liveness(0, 1), Liveness::Unknown);
}

#[tokio::test]
async fn kill_tree_does_not_kill_reused_pid() {
    let (_, clock) = clock();
    let runner = FakeProcessRunner::new(clock);
    let inspector = runner.processes();
    inspector.add(ProcessRef {
        pid: 300,
        start: Some(2),
    });
    runner
        .kill_tree(&ProcessRef {
            pid: 300,
            start: Some(1),
        })
        .await
        .unwrap();
    assert_eq!(inspector.liveness(300, 2), Liveness::Alive);
}
