mod common;

use common::{
    clock, fresh_file, helper_command, job, real_sleep, step, workdir, Helper, Recorder, T0,
};
use lr_abstractions::{
    stop_signal, Clock, JobEvent, JobOutcome, JobSpec, ProcessRunner, SecretValue, Stop,
};
use lr_core::{JobState, StepState, Timestamp};
use lr_exec::{header_line, ExecRunner, SystemClock};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

#[cfg(windows)]
const NO_NEWLINE_ABC: &str = "<nul set /p =abc& exit 0";
#[cfg(unix)]
const NO_NEWLINE_ABC: &str = "printf abc";

#[cfg(windows)]
fn show(var: &str) -> String {
    format!("echo [%{var}%]")
}
#[cfg(unix)]
fn show(var: &str) -> String {
    format!("echo \"[${var}]\"")
}

#[cfg(windows)]
fn unset_shown(var: &str) -> String {
    format!("[%{var}%]")
}
#[cfg(unix)]
fn unset_shown(_var: &str) -> String {
    "[]".to_string()
}

#[cfg(windows)]
const WRITE_MARKER: &str = "echo ok> marker.txt";
#[cfg(unix)]
const WRITE_MARKER: &str = "echo ok > marker.txt";

fn states(outcome: &JobOutcome) -> Vec<StepState> {
    outcome.steps.iter().map(|s| s.state).collect()
}

async fn run(spec: JobSpec) -> (JobOutcome, Vec<JobEvent>) {
    let runner = ExecRunner::new(Arc::new(SystemClock));
    let out = Recorder::new();
    let (_handle, stop) = stop_signal();
    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        runner.run_job(spec, out.as_ref(), stop),
    )
    .await
    .expect("run_job did not return");
    (outcome, out.events())
}

fn start(spec: JobSpec, clock: Arc<dyn Clock>) -> (JoinHandle<JobOutcome>, Arc<Recorder>) {
    let runner = ExecRunner::new(clock);
    let out = Recorder::new();
    let (_handle, stop) = stop_signal();
    let o = out.clone();
    let task = tokio::spawn(async move {
        let _keep = _handle;
        runner.run_job(spec, o.as_ref(), stop).await
    });
    (task, out)
}

fn text(events: &[JobEvent]) -> String {
    common::events_text(events)
}

#[tokio::test]
async fn failed_step_skips_rest_unless_continue_on_failure() {
    let spec = job(
        "f",
        60,
        vec![step("a", "exit 3", 30), step("b", "echo never", 30)],
    );
    let (outcome, events) = run(spec).await;
    assert_eq!(outcome.state, JobState::Failed);
    assert_eq!(
        states(&outcome),
        vec![StepState::Failed, StepState::Skipped]
    );
    assert_eq!(outcome.steps[0].exit_code, Some(3));
    assert_eq!(outcome.exit_code, Some(3));
    assert_eq!(outcome.steps[1].started_at, None);
    assert_eq!(outcome.steps[1].finished_at, None);
    assert!(!text(&events).contains("never"));
    assert!(!events
        .iter()
        .any(|e| matches!(e, JobEvent::StepStarted { index: 1, .. })));

    let mut first = step("a", "exit 3", 30);
    first.continue_on_failure = true;
    let spec = job(
        "f2",
        60,
        vec![
            first,
            step("b", "echo ran", 30),
            step("c", "echo after", 30),
        ],
    );
    let (outcome, events) = run(spec).await;
    assert_eq!(outcome.state, JobState::Failed);
    assert_eq!(
        states(&outcome),
        vec![StepState::Failed, StepState::Passed, StepState::Passed]
    );
    assert_eq!(outcome.exit_code, Some(0));
    assert!(text(&events).contains("ran") && text(&events).contains("after"));
}

#[tokio::test]
async fn all_steps_passing_pass_the_job_with_ordered_events() {
    let spec = job(
        "p",
        60,
        vec![step("a", "echo one", 30), step("b", "exit 0", 30)],
    );
    let (outcome, events) = run(spec).await;
    assert_eq!(outcome.state, JobState::Passed);
    assert_eq!(states(&outcome), vec![StepState::Passed, StepState::Passed]);
    assert!(outcome
        .steps
        .iter()
        .all(|s| s.started_at.is_some() && s.finished_at >= s.started_at));
    assert!(matches!(&events[0], JobEvent::StepStarted { index: 0, name, .. } if name == "a"));
    assert!(matches!(
        events.last(),
        Some(JobEvent::StepFinished { index: 1, outcome }) if outcome.state == StepState::Passed
    ));
    let starts = events
        .iter()
        .position(|e| matches!(e, JobEvent::StepStarted { index: 1, .. }))
        .unwrap();
    let first_done = events
        .iter()
        .position(|e| matches!(e, JobEvent::StepFinished { index: 0, .. }))
        .unwrap();
    assert!(first_done < starts);
}

#[tokio::test]
async fn exit_code_of_the_process_is_reported() {
    let (outcome, _) = run(job("e", 60, vec![step("a", "exit 7", 30)])).await;
    assert_eq!(outcome.state, JobState::Failed);
    assert_eq!(outcome.steps[0].exit_code, Some(7));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step_deadline_is_min_of_step_and_remaining_job() {
    let (manual, clock) = clock();
    let cmd = helper_command(&Helper::new("ignore"));
    let (task, out) = start(job("j1", 10, vec![step("s", &cmd, 100)]), clock.clone());
    out.pid_of("child").await;
    manual.advance(Duration::from_millis(9_900));
    real_sleep(400).await;
    assert!(!task.is_finished(), "killed before the job timeout");
    manual.advance(Duration::from_millis(100));
    let outcome = tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.state, JobState::Timeout);
    assert_eq!(outcome.steps[0].state, StepState::Timeout);

    let (manual, clock) = common::clock();
    let spec = job(
        "j2",
        100,
        vec![step("s", &cmd, 5), step("b", "echo never", 30)],
    );
    let (task, out) = start(spec, clock);
    out.pid_of("child").await;
    manual.advance(Duration::from_millis(4_900));
    real_sleep(400).await;
    assert!(!task.is_finished(), "killed before the step timeout");
    manual.advance(Duration::from_millis(100));
    let outcome = tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        states(&outcome),
        vec![StepState::Timeout, StepState::Skipped]
    );
    assert_eq!(outcome.state, JobState::Timeout);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn later_step_gets_only_the_time_left_of_the_job() {
    let (manual, clock) = clock();
    let gate = fresh_file("job", "remaining.gate");
    let first = helper_command(&Helper::new("wait-file").gate(&gate));
    let second = helper_command(&Helper::new("ignore"));
    let spec = job(
        "r",
        10,
        vec![step("a", &first, 100), step("b", &second, 100)],
    );
    let (task, out) = start(spec, clock);
    out.pid_of("child").await;
    manual.advance(Duration::from_secs(4));
    std::fs::write(&gate, b"go").unwrap();
    while out.text().matches("child-pid-end").count() < 2 {
        out.wait_for("child-pid-end").await;
        real_sleep(20).await;
    }
    manual.advance(Duration::from_millis(5_900));
    real_sleep(400).await;
    assert!(
        !task.is_finished(),
        "second step killed before the job ran out"
    );
    manual.advance(Duration::from_millis(100));
    let outcome = tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        states(&outcome),
        vec![StepState::Passed, StepState::Timeout]
    );
    assert_eq!(outcome.state, JobState::Timeout);
    assert_eq!(outcome.steps[0].started_at, Some(T0));
    assert_eq!(outcome.steps[0].finished_at, Some(Timestamp(T0.0 + 4_000)));
    assert_eq!(outcome.steps[1].started_at, Some(Timestamp(T0.0 + 4_000)));
    assert_eq!(outcome.steps[1].finished_at, Some(Timestamp(T0.0 + 10_000)));
}

#[tokio::test]
async fn step_reached_with_no_time_left_times_out_without_starting() {
    let spec = job(
        "z",
        0,
        vec![step("a", "echo never", 30), step("b", "echo never", 30)],
    );
    let (outcome, events) = run(spec).await;
    assert_eq!(outcome.state, JobState::Timeout);
    assert_eq!(
        states(&outcome),
        vec![StepState::Timeout, StepState::Skipped]
    );
    assert_eq!(outcome.steps[0].started_at, None);
    assert!(events.is_empty());
}

#[tokio::test]
async fn stop_before_the_first_step_skips_everything() {
    let runner = ExecRunner::new(Arc::new(SystemClock));
    let out = Recorder::new();
    let (handle, stop) = stop_signal();
    handle.stop(Stop::Cancel);
    let spec = job("s", 60, vec![step("a", "echo never", 30)]);
    let outcome = runner.run_job(spec, out.as_ref(), stop).await;
    assert_eq!(outcome.state, JobState::Cancelled);
    assert_eq!(states(&outcome), vec![StepState::Skipped]);
    assert!(out.events().is_empty());
}

#[tokio::test]
async fn step_that_cannot_be_spawned_fails_without_exit_code() {
    let mut spec = job(
        "x",
        60,
        vec![step("a", "echo hi", 30), step("b", "echo never", 30)],
    );
    spec.cwd = workdir("job").join("does-not-exist");
    let (outcome, events) = run(spec).await;
    assert_eq!(outcome.state, JobState::Failed);
    assert_eq!(
        states(&outcome),
        vec![StepState::Failed, StepState::Skipped]
    );
    assert_eq!(outcome.steps[0].exit_code, None);
    assert_eq!(outcome.exit_code, None);
    assert!(matches!(
        events.last(),
        Some(JobEvent::StepFinished { index: 0, .. })
    ));
}

#[tokio::test]
async fn steps_run_in_the_job_directory_with_spaces_and_cyrillic() {
    let dir = workdir("папка с пробелом");
    let _ = std::fs::remove_file(dir.join("marker.txt"));
    let mut spec = job("cwd", 60, vec![step("a", WRITE_MARKER, 30)]);
    spec.cwd = dir.clone();
    let (outcome, _) = run(spec).await;
    assert_eq!(outcome.state, JobState::Passed);
    assert!(dir.join("marker.txt").exists());
}

#[tokio::test]
async fn environment_is_only_the_job_env_and_cache_vars_reach_the_step() {
    assert!(std::env::var("CARGO_MANIFEST_DIR").is_ok());
    let mut spec = job(
        "env",
        60,
        vec![
            step("a", &show("CARGO_MANIFEST_DIR"), 30),
            step("b", &show("LOCALRUNNER_CACHE_GRADLE"), 30),
        ],
    );
    spec.env.push((
        "LOCALRUNNER_CACHE_GRADLE".to_string(),
        "cache-dir-1".to_string(),
    ));
    let (outcome, events) = run(spec).await;
    assert_eq!(outcome.state, JobState::Passed);
    let text = text(&events);
    assert!(text.contains(&unset_shown("CARGO_MANIFEST_DIR")), "{text}");
    assert!(text.contains("[cache-dir-1]"), "{text}");
}

#[cfg(windows)]
#[tokio::test]
async fn windows_system_variables_reach_the_step_from_the_runner_environment() {
    let names = ["SystemRoot", "windir", "ComSpec", "PATHEXT"];
    let steps = names
        .iter()
        .map(|n| step(&n.to_lowercase(), &show(n), 30))
        .collect();
    let spec = job("os", 60, steps);
    assert!(spec
        .env
        .iter()
        .all(|(k, _)| !names.iter().any(|n| k.eq_ignore_ascii_case(n))));
    let (outcome, events) = run(spec).await;
    assert_eq!(outcome.state, JobState::Passed);
    let text = text(&events);
    for n in names {
        let value = std::env::var(n).expect("set on Windows");
        assert!(text.contains(&format!("[{value}]")), "{n}: {text}");
    }
}

#[tokio::test]
async fn secrets_reach_only_their_step_and_are_masked() {
    let mut spec = job(
        "sec",
        60,
        vec![step("a", &show("TOKEN"), 30), step("b", &show("TOKEN"), 30)],
    );
    spec.steps[0].secrets = vec![("TOKEN".to_string(), SecretValue::new("s3cr3t-value"))];
    let (outcome, events) = run(spec).await;
    assert_eq!(outcome.state, JobState::Passed);
    let text = text(&events);
    assert!(!text.contains("s3cr3t-value"), "{text}");
    assert!(text.contains("[***]"), "{text}");
    assert!(text.contains(&unset_shown("TOKEN")), "{text}");
}

#[tokio::test]
async fn each_step_output_starts_with_a_header_line_with_job_fields() {
    let mut spec = job(
        "h",
        60,
        vec![
            step("alpha", NO_NEWLINE_ABC, 30),
            step("beta", "echo two", 30),
        ],
    );
    let fields: Vec<(String, String)> = [
        ("project", "owner/repo"),
        ("source", "session"),
        ("label", "claude:fix-12"),
        ("note", "why s3cr3t"),
        ("branch", "main"),
        ("cwd", "D:/work"),
        ("cancel", "bob at 12:00: stale"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    spec.header = fields.clone();
    spec.steps[1].secrets = vec![("T".to_string(), SecretValue::new("s3cr3t"))];
    let (outcome, events) = run(spec).await;
    assert_eq!(outcome.state, JobState::Passed);
    let first_output = events
        .iter()
        .find_map(|e| match e {
            JobEvent::Output { index: 0, data } => Some(data.clone()),
            _ => None,
        })
        .unwrap();
    let masked: Vec<(String, String)> = fields
        .iter()
        .map(|(k, v)| (k.clone(), v.replace("s3cr3t", "***")))
        .collect();
    assert_eq!(first_output, header_line("alpha", &masked).into_bytes());
    let text = text(&events);
    for (k, v) in &masked {
        assert!(text.contains(&format!("{k}={v}")), "{k} missing in {text}");
    }
    assert!(!text.contains("s3cr3t"));
    let beta = header_line("beta", &masked);
    assert!(text.contains(&format!("abc\n{beta}")), "{text}");
    assert_eq!(text.matches("=== step ").count(), 2);
}
