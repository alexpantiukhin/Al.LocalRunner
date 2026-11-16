mod common;

use common::{helper_command, job, step, Helper, Recorder, HANG};
use lr_abstractions::{stop_signal, Clock, JobOutcome, JobSpec, ProcessRunner, Stop, StopHandle};
use lr_core::{JobState, StepState};
use lr_exec::{ExecRunner, SystemClock};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PAD: &str = "0123456789012345678901234567890123456789";

fn flood(n: u64) -> String {
    helper_command(&Helper::new("flood").count(n))
}

async fn run_blocked(
    spec: JobSpec,
    out: Arc<Recorder>,
    clock: Arc<dyn Clock>,
    when_blocked: impl FnOnce(&StopHandle),
) -> JobOutcome {
    let runner = ExecRunner::new(clock);
    let (handle, stop) = stop_signal();
    let react = async {
        out.until_blocked().await;
        when_blocked(&handle);
    };
    let both = async { tokio::join!(runner.run_job(spec, out.as_ref(), stop), react) };
    let (outcome, ()) = tokio::time::timeout(HANG, both)
        .await
        .expect("run_job waited for a blocked consumer");
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_consumer_loses_nothing_and_keeps_order() {
    let out = Recorder::new().slow_first(20);
    let spec = job("bp", 120, vec![step("flood", &flood(20_000), 100)]);
    let runner = ExecRunner::new(Arc::new(SystemClock));
    let (_handle, stop) = stop_signal();
    let outcome = tokio::time::timeout(HANG, runner.run_job(spec, out.as_ref(), stop))
        .await
        .expect("run_job did not return");
    assert_eq!(outcome.state, JobState::Passed);
    let numbers: Vec<u64> = out
        .text()
        .lines()
        .filter(|l| l.starts_with(PAD))
        .map(|l| l.rsplit(' ').next().unwrap().trim().parse().unwrap())
        .collect();
    assert_eq!(numbers, (1..=20_000).collect::<Vec<u64>>());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_is_prompt_even_if_the_consumer_is_blocked() {
    let (manual, clock) = common::clock();
    let out = Recorder::new().block_after(3);
    let spec = job(
        "bt",
        120,
        vec![
            step("flood", &flood(50_000_000), 1),
            step("after", "echo never", 30),
        ],
    );
    let started = Instant::now();
    let outcome = run_blocked(spec, out, clock, |_| {
        manual.advance(Duration::from_secs(1));
    })
    .await;
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(outcome.state, JobState::Timeout);
    assert_eq!(outcome.steps[0].state, StepState::Timeout);
    assert_eq!(outcome.steps[1].state, StepState::Skipped);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_is_prompt_even_if_the_consumer_is_blocked() {
    let out = Recorder::new().block_after(3);
    let mut spec = job(
        "bc",
        120,
        vec![
            step("flood", &flood(50_000_000), 100),
            step("after", "echo never", 30),
        ],
    );
    spec.cancel_grace = Duration::from_millis(500);
    let started = Instant::now();
    let outcome = run_blocked(spec, out, Arc::new(SystemClock), |stop| {
        stop.stop(Stop::Cancel);
    })
    .await;
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(outcome.state, JobState::Cancelled);
    assert_eq!(outcome.steps[0].state, StepState::Cancelled);
    assert_eq!(outcome.steps[1].state, StepState::Skipped);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_is_prompt_when_the_consumer_blocks_before_the_step_starts() {
    let out = Recorder::new().block_after(0);
    let spec = job(
        "bs",
        120,
        vec![
            step("flood", &flood(50_000_000), 100),
            step("after", "echo never", 30),
        ],
    );
    let started = Instant::now();
    let outcome = run_blocked(spec, out, Arc::new(SystemClock), |stop| {
        stop.stop(Stop::Cancel);
    })
    .await;
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(outcome.state, JobState::Cancelled);
    assert_eq!(outcome.steps[0].state, StepState::Cancelled);
    assert_eq!(outcome.steps[1].state, StepState::Skipped);
}
