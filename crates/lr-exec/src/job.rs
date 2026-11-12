use crate::limits::kill_line;
use crate::log::{cancel_line, header_line, SOFT_STOP_NOT_DELIVERED};
use crate::spawn::{spawn_step, Launch};
use crate::step::{supervise, Emitter, Ended, Limits};
use lr_abstractions::{
    mask_secrets, Clock, Exceeded, JobEvent, JobOutcome, JobOutput, JobSpec, JobStep, ProcessRef,
    SecretValue, StepOutcome, StopSignal, Usage,
};
use lr_core::{JobState, StepState};
use std::time::Duration;

struct Job<'a> {
    spec: &'a JobSpec,
    clock: &'a dyn Clock,
    secrets: Vec<&'a SecretValue>,
    emitter: Emitter<'a>,
    usage: Usage,
    exceeded: Option<Exceeded>,
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

    async fn line(&mut self, index: usize, text: &str, stop: &mut StopSignal) {
        let mut line = Vec::new();
        if self.emitter.line_open {
            line.push(b'\n');
        }
        line.extend_from_slice(text.as_bytes());
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
            limits: &self.spec.limits,
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
        if let Some(start) = spawned.tree.start() {
            let process = ProcessRef {
                pid: spawned.child.id().unwrap_or(0),
                start: Some(start),
            };
            let event = JobEvent::StepProcess { index, process };
            self.emitter.send(event, stop).await;
        }
        let limits = Limits {
            deadline,
            grace: self.spec.cancel_grace,
            clock: self.clock,
            resources: &self.spec.limits,
        };
        let finished = supervise(spawned, limits, index, stop, &mut self.emitter).await;
        if finished.soft_failed {
            let text = format!("{SOFT_STOP_NOT_DELIVERED}; killed after the grace\n");
            self.line(index, &text, stop).await;
        }
        self.usage.peak_mem_mb = self.usage.peak_mem_mb.max(finished.usage.peak_mem_mb);
        self.usage.peak_disk_mb = self.usage.peak_disk_mb.max(finished.usage.peak_disk_mb);
        if let Some(ex) = finished.exceeded {
            self.line(index, &kill_line(&ex), stop).await;
            self.exceeded.get_or_insert(ex);
        }
        match finished.ended {
            Ended::Exceeded => (StepState::Failed, None),
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
        usage: Usage::default(),
        exceeded: None,
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
            exit_code = None;
            halted = true;
            steps.push(not_run(step, StepState::Skipped));
            continue;
        }
        let now = clock.now();
        if now >= budget_end {
            state = JobState::Timeout;
            exit_code = None;
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
        let header = header_line(&step.spec.name, &spec.header);
        job.line(index, &header, &mut stop).await;
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
        if step_state == StepState::Cancelled {
            let reason = stop.reason();
            let trailer = cancel_line(&step.spec.name, clock.now(), reason.as_deref());
            job.line(index, &trailer, &mut stop).await;
        }
        match step_state {
            StepState::Failed => {
                state = JobState::Failed;
                halted = !step.spec.continue_on_failure || job.exceeded.is_some();
            }
            StepState::Timeout => {
                state = JobState::Timeout;
                exit_code = code;
                halted = true;
            }
            StepState::Cancelled => {
                state = JobState::Cancelled;
                exit_code = code;
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
        usage: job.usage,
        exceeded: job.exceeded,
    }
}
