use lr_cli::commands::wait_events::{event_line, line_json, needs_reexplain};
use lr_core::{JobId, JobState, StepState};
use lr_proto::{QueueEvent, StepView};

fn step(name: &str, state: StepState) -> StepView {
    StepView {
        ord: 0,
        name: name.into(),
        state,
        exit_code: None,
        started_at: None,
        finished_at: None,
        timeout_s: 60,
    }
}

#[test]
fn the_events_of_the_job_become_one_line_each_and_other_jobs_say_nothing() {
    let job = JobId("j1".into());
    let other = JobId("j2".into());
    let started = QueueEvent::JobStarted {
        job: job.clone(),
        runner: "r1".into(),
    };
    assert_eq!(event_line(&started, &job).unwrap(), "running on runner r1");
    assert!(event_line(&started, &other).is_none());
    let running = QueueEvent::StepChanged {
        job: job.clone(),
        step: step("build", StepState::Running),
    };
    assert_eq!(event_line(&running, &job).unwrap(), "step build");
    let passed = QueueEvent::StepChanged {
        job: job.clone(),
        step: step("build", StepState::Passed),
    };
    assert!(
        event_line(&passed, &job).is_none(),
        "a step is reported once, when it starts"
    );
    let finished = QueueEvent::JobFinished {
        job: job.clone(),
        state: JobState::Failed,
        reason: None,
    };
    assert_eq!(event_line(&finished, &job).unwrap(), "finished: failed");
}

#[test]
fn the_queue_text_is_asked_again_when_the_queue_or_the_runners_changed() {
    let finished = QueueEvent::JobFinished {
        job: JobId("j2".into()),
        state: JobState::Passed,
        reason: None,
    };
    assert!(needs_reexplain(&finished));
    assert!(needs_reexplain(&QueueEvent::RunnerOnline {
        name: "r".into()
    }));
    assert!(needs_reexplain(&QueueEvent::RunnerOffline {
        name: "r".into()
    }));
    let step = QueueEvent::StepChanged {
        job: JobId("j2".into()),
        step: step("x", StepState::Running),
    };
    assert!(!needs_reexplain(&step));
}

#[test]
fn a_json_line_carries_the_job_and_the_text() {
    let line = line_json(&JobId("j1".into()), "running on runner r1");
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["job"], "j1");
    assert_eq!(value["event"], "running on runner r1");
}
