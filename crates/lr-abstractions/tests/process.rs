mod common;

use common::{clock, job, step, T0};
use lr_abstractions::fakes::{FakeProcessRunner, JobScript, RecordingOutput, StepScript};
use lr_abstractions::{stop_signal, JobEvent, JobOutcome, JobSpec, ProcessRunner, SecretValue};
use lr_core::{JobState, StepState};
use std::sync::Arc;
use std::time::Duration;

async fn run(
    runner: Arc<FakeProcessRunner>,
    manual: &lr_abstractions::fakes::ManualClock,
    spec: JobSpec,
    total: Duration,
) -> (JobOutcome, Arc<RecordingOutput>) {
    let output = Arc::new(RecordingOutput::new());
    let (_handle, stop) = stop_signal();
    let o = output.clone();
    let task = tokio::spawn(async move { runner.run_job(spec, o.as_ref(), stop).await });
    let mut left = total;
    loop {
        common::settle().await;
        if task.is_finished() || left.is_zero() {
            break;
        }
        manual.advance(Duration::from_secs(1));
        left = left.saturating_sub(Duration::from_secs(1));
    }
    (task.await.unwrap(), output)
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[tokio::test]
async fn failed_step_skips_rest_and_fails_job() {
    let (manual, clock) = clock();
    let runner = Arc::new(FakeProcessRunner::new(clock));
    let spec = job("j", 100, vec![step("a", 10), step("b", 10), step("c", 10)]);
    runner.script(
        spec.job.clone(),
        JobScript::new(vec![
            StepScript::new(secs(1), 0),
            StepScript::new(secs(1), 3),
        ]),
    );
    let (outcome, _) = run(runner, &manual, spec, secs(10)).await;
    assert_eq!(outcome.state, JobState::Failed);
    assert_eq!(outcome.exit_code, Some(3));
    let states: Vec<_> = outcome.steps.iter().map(|s| s.state).collect();
    assert_eq!(
        states,
        vec![StepState::Passed, StepState::Failed, StepState::Skipped]
    );
    assert_eq!(outcome.steps[2].started_at, None);
}

#[tokio::test]
async fn continue_on_failure_runs_next_step_but_job_stays_failed() {
    let (manual, clock) = clock();
    let runner = Arc::new(FakeProcessRunner::new(clock));
    let mut first = step("a", 10);
    first.continue_on_failure = true;
    let spec = job("j", 100, vec![first, step("b", 10)]);
    runner.script(
        spec.job.clone(),
        JobScript::new(vec![
            StepScript::new(secs(1), 1),
            StepScript::new(secs(1), 0),
        ]),
    );
    let (outcome, _) = run(runner, &manual, spec, secs(10)).await;
    assert_eq!(outcome.state, JobState::Failed);
    assert_eq!(outcome.steps[1].state, StepState::Passed);
}

#[tokio::test]
async fn step_timeout_kills_at_step_deadline_and_skips_rest() {
    let (manual, clock) = clock();
    let runner = Arc::new(FakeProcessRunner::new(clock));
    let spec = job("j", 100, vec![step("a", 3), step("b", 10)]);
    runner.script(
        spec.job.clone(),
        JobScript::new(vec![StepScript::new(secs(50), 0)]),
    );
    let (outcome, _) = run(runner, &manual, spec, secs(20)).await;
    assert_eq!(outcome.state, JobState::Timeout);
    assert_eq!(outcome.steps[0].state, StepState::Timeout);
    assert_eq!(outcome.steps[0].finished_at, Some(T0.plus_secs(3)));
    assert_eq!(outcome.steps[1].state, StepState::Skipped);
}

#[tokio::test]
async fn step_deadline_is_min_of_step_timeout_and_remaining_job_budget() {
    let (manual, clock) = clock();
    let runner = Arc::new(FakeProcessRunner::new(clock));
    let spec = job("j", 5, vec![step("a", 10), step("b", 10)]);
    runner.script(
        spec.job.clone(),
        JobScript::new(vec![
            StepScript::new(secs(3), 0),
            StepScript::new(secs(9), 0),
        ]),
    );
    let (outcome, _) = run(runner, &manual, spec, secs(20)).await;
    assert_eq!(outcome.state, JobState::Timeout);
    assert_eq!(outcome.steps[1].state, StepState::Timeout);
    assert_eq!(outcome.steps[1].finished_at, Some(T0.plus_secs(5)));
}

#[tokio::test]
async fn events_report_step_boundaries_and_output_in_order() {
    let (manual, clock) = clock();
    let runner = Arc::new(FakeProcessRunner::new(clock));
    let spec = job("j", 100, vec![step("a", 10), step("b", 10)]);
    runner.script(
        spec.job.clone(),
        JobScript::new(vec![
            StepScript::new(secs(1), 0).output(b"hello\n"),
            StepScript::new(secs(1), 0).output(b"bye\n"),
        ]),
    );
    let (_, output) = run(runner, &manual, spec, secs(10)).await;
    let events = output.events();
    assert_eq!(events.len(), 6);
    assert!(matches!(&events[0], JobEvent::StepStarted { index: 0, name, .. } if name == "a"));
    assert!(matches!(&events[1], JobEvent::Output { index: 0, data } if data == b"hello\n"));
    assert!(matches!(
        &events[2],
        JobEvent::StepFinished { index: 0, .. }
    ));
    assert!(matches!(&events[3], JobEvent::StepStarted { index: 1, .. }));
    assert_eq!(output.text(), "hello\nbye\n");
}

#[tokio::test]
async fn secret_values_are_masked_in_job_output() {
    let (manual, clock) = clock();
    let runner = Arc::new(FakeProcessRunner::new(clock));
    let mut spec = job("j", 100, vec![step("publish", 10)]);
    spec.steps[0].secrets = vec![("TOKEN".into(), SecretValue::new("s3cr3t"))];
    runner.script(
        spec.job.clone(),
        JobScript::new(vec![
            StepScript::new(secs(1), 0).output(b"token=s3cr3t; again s3cr3t")
        ]),
    );
    let (_, output) = run(runner, &manual, spec, secs(5)).await;
    assert_eq!(output.text(), "token=***; again ***");
}

#[tokio::test]
async fn unscripted_job_passes_every_step_instantly_and_spec_is_recorded() {
    let (manual, clock) = clock();
    let runner = Arc::new(FakeProcessRunner::new(clock));
    let spec = job("j", 100, vec![step("a", 10), step("b", 10)]);
    let (outcome, _) = run(runner.clone(), &manual, spec.clone(), secs(1)).await;
    assert_eq!(outcome.state, JobState::Passed);
    assert_eq!(outcome.steps.len(), 2);
    assert_eq!(runner.runs().len(), 1);
    assert_eq!(runner.runs()[0].job, spec.job);
}

#[test]
fn job_spec_defaults_cancel_grace_to_10_seconds() {
    let spec = job("j", 100, vec![step("a", 10)]);
    assert_eq!(spec.cancel_grace, Duration::from_secs(10));
}
