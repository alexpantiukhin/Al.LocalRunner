use crate::log::header_line;
use crate::spawn::{spawn_step, Launch};
use crate::step::{supervise, Emitter, Ended, Limits};
use lr_abstractions::{
    mask_secrets, Clock, JobEvent, JobOutcome, JobOutput, JobSpec, JobStep, SecretValue,
    StepOutcome, StopSignal,
};
use lr_core::{JobState, StepState};
use std::time::Duration;

struct Job<'a> {
    spec: &'a JobSpec,
    clock: &'a dyn Clock,
    secrets: Vec<&'a SecretValue>,
    emitter: Emitter<'a>,
}

fn not_run(step: &JobStep, state: StepState) -> StepOutcome {
    StepOutcome {
        name: step.spec.name.clone(),
        state,
        exit_code: None,
        started_at: None,
        finished_at: None,
    }
}

impl<'a> Job<'a> {
    async fn output(&mut self, index: usize, data: Vec<u8>, stop: &mut StopSignal) {
        self.emitter
            .send(JobEvent::Output { index, data }, stop)
            .await;
    }

    async fn header(&mut self, index: usize, step: &JobStep, stop: &mut StopSignal) {
        let mut line = Vec::new();
        if self.emitter.line_open {
            line.push(b'\n');
        }
        line.extend(header_line(&step.spec.name, &self.spec.header).into_bytes());
        let line = mask_secrets(&line, &self.secrets);
        self.output(index, line, stop).await;
    }

    async fn run_step(
        &mut self,
        index: usize,
        step: &'a JobStep,
        limit: Duration,
        stop: &mut StopSignal,
    ) -> (StepState, Option<i32>) {
        let deadline = self.clock.sleep(limit);
        let launch = Launch {
            command: &step.spec.command,
            cwd: &self.spec.cwd,
            env: &self.spec.env,
            secrets: &step.secrets,
        };
        if stop.current().is_some() {
            return (StepState::Cancelled, None);
        }
        let spawned = match spawn_step(&launch, &self.secrets) {
            Ok(spawned) => spawned,
            Err(err) => {
                let text = format!(
                    "localrunner: step `{}` not started: {err}\n",
                    step.spec.name
                );
                let text = mask_secrets(text.as_bytes(), &self.secrets);
                self.output(index, text, stop).await;
                return (StepState::Failed, None);
            }
        };
        let limits = Limits {
            deadline,
            grace: self.spec.cancel_grace,
            clock: self.clock,
        };
        match supervise(spawned, limits, index, stop, &mut self.emitter).await {
            Ended::Exited(0) => (StepState::Passed, Some(0)),
            Ended::Exited(code) => (StepState::Failed, Some(code)),
            Ended::Timeout => (StepState::Timeout, None),
            Ended::Cancelled => (StepState::Cancelled, None),
        }
    }
}

pub(crate) async fn run_job(
    spec: &JobSpec,
    clock: &dyn Clock,
    output: &dyn JobOutput,
    mut stop: StopSignal,
) -> JobOutcome {
    let mut job = Job {
        spec,
        clock,
        secrets: spec.secret_values(),
        emitter: Emitter::new(output),
    };
    let budget_end = clock.now().plus_secs(spec.timeout_s);
    let mut state = JobState::Passed;
    let mut exit_code = None;
    let mut halted = false;
    let mut steps = Vec::with_capacity(spec.steps.len());
    for (index, step) in spec.steps.iter().enumerate() {
        if halted {
            steps.push(not_run(step, StepState::Skipped));
            continue;
        }
        if stop.current().is_some() {
            state = JobState::Cancelled;
            halted = true;
            steps.push(not_run(step, StepState::Skipped));
            continue;
        }
        let now = clock.now();
        if now >= budget_end {
            state = JobState::Timeout;
            halted = true;
            steps.push(not_run(step, StepState::Timeout));
            continue;
        }
        let limit = Duration::from_millis(budget_end.millis_since(now))
            .min(Duration::from_secs(step.spec.timeout_s));
        let started = JobEvent::StepStarted {
            index,
            name: step.spec.name.clone(),
            at: now,
        };
        job.emitter.send(started, &mut stop).await;
        job.header(index, step, &mut stop).await;
        let (step_state, code) = job.run_step(index, step, limit, &mut stop).await;
        let outcome = StepOutcome {
            name: step.spec.name.clone(),
            state: step_state,
            exit_code: code,
            started_at: Some(now),
            finished_at: Some(clock.now()),
        };
        if code.is_some() {
            exit_code = code;
        }
        match step_state {
            StepState::Failed => {
                state = JobState::Failed;
                halted = !step.spec.continue_on_failure;
            }
            StepState::Timeout => {
                state = JobState::Timeout;
                halted = true;
            }
            StepState::Cancelled => {
                state = JobState::Cancelled;
                halted = true;
            }
            _ => {}
        }
        let finished = JobEvent::StepFinished {
            index,
            outcome: outcome.clone(),
        };
        job.emitter.send(finished, &mut stop).await;
        steps.push(outcome);
    }
    JobOutcome {
        state,
        exit_code,
        steps,
    }
}
