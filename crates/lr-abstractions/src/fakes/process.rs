use crate::clock::Clock;
use crate::process::{
    mask_secrets, Exceeded, JobEvent, JobOutcome, JobOutput, JobSpec, Liveness, ProcessError,
    ProcessInspector, ProcessRef, ProcessRunner, SpawnSpec, StepOutcome, Stop, StopSignal, Usage,
};
use async_trait::async_trait;
use lr_core::{JobId, JobState, StepState};
use std::collections::HashMap;
use std::future::Future;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

/// The pid the fake reports for the root of step 0 (step `i` gets this plus `i`), start 1.
pub const FAKE_STEP_PID: u32 = 50_000;

/// How one scripted step behaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepScript {
    /// Time until the process exits by itself.
    pub duration: Duration,
    /// Its exit code.
    pub exit_code: i32,
    /// Output written when the step starts.
    pub output: Vec<u8>,
    /// Whether it exits at once on the soft signal; otherwise it is killed after the grace.
    pub obeys_soft_stop: bool,
}

impl StepScript {
    /// A step that exits with `exit_code` after `duration` and obeys the soft signal.
    pub fn new(duration: Duration, exit_code: i32) -> StepScript {
        StepScript {
            duration,
            exit_code,
            output: Vec::new(),
            obeys_soft_stop: true,
        }
    }

    /// Sets the output written at start.
    pub fn output(mut self, data: &[u8]) -> StepScript {
        self.output = data.to_vec();
        self
    }

    /// Makes the step ignore the soft signal, so a cancel waits for the grace.
    pub fn ignores_soft_stop(mut self) -> StepScript {
        self.obeys_soft_stop = false;
        self
    }
}

/// Script of one job: one entry per step; missing steps exit 0 at once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobScript {
    /// Steps in order.
    pub steps: Vec<StepScript>,
    /// Peaks the job reports.
    pub usage: Usage,
    /// The ceiling the job reports as exceeded; the job then ends `Failed`.
    pub exceeded: Option<Exceeded>,
}

impl JobScript {
    /// A script from its steps.
    pub fn new(steps: Vec<StepScript>) -> JobScript {
        JobScript {
            steps,
            usage: Usage::default(),
            exceeded: None,
        }
    }

    /// Sets the peaks the job reports.
    pub fn usage(mut self, usage: Usage) -> JobScript {
        self.usage = usage;
        self
    }

    /// Makes the job report `exceeded` and end `Failed`.
    pub fn exceeded(mut self, exceeded: Exceeded) -> JobScript {
        self.exceeded = Some(exceeded);
        self
    }
}

/// A process table of this machine: pid to start fingerprint.
#[derive(Clone, Default)]
pub struct FakeProcessInspector {
    table: Arc<Mutex<HashMap<u32, Option<u64>>>>,
}

impl FakeProcessInspector {
    /// An empty table.
    pub fn new() -> FakeProcessInspector {
        FakeProcessInspector::default()
    }

    /// A process now runs.
    pub fn add(&self, process: ProcessRef) {
        self.table
            .lock()
            .unwrap()
            .insert(process.pid, process.start);
    }

    /// The process with this pid is gone.
    pub fn remove(&self, pid: u32) {
        self.table.lock().unwrap().remove(&pid);
    }
}

impl ProcessInspector for FakeProcessInspector {
    fn liveness(&self, pid: u32, start: u64) -> Liveness {
        if pid == 0 {
            return Liveness::Unknown;
        }
        match self.table.lock().unwrap().get(&pid) {
            None => Liveness::Dead,
            Some(None) => Liveness::Unknown,
            Some(Some(s)) if *s == start => Liveness::Alive,
            Some(Some(_)) => Liveness::Dead,
        }
    }
}

#[derive(Default)]
struct Calls {
    scripts: HashMap<JobId, JobScript>,
    runs: Vec<JobSpec>,
    spawns: Vec<SpawnSpec>,
    kills: Vec<ProcessRef>,
    refused: Vec<String>,
    next_pid: u32,
}

/// A process runner that executes scripts on a clock instead of processes, following the
/// [`ProcessRunner`] contract (timeouts, failure skipping, cancel grace, masking).
pub struct FakeProcessRunner {
    clock: Arc<dyn Clock>,
    calls: Mutex<Calls>,
    processes: FakeProcessInspector,
}

enum Waited {
    Elapsed,
    Stopped(Stop),
}

impl FakeProcessRunner {
    /// A runner timing its steps on `clock`.
    pub fn new(clock: Arc<dyn Clock>) -> FakeProcessRunner {
        FakeProcessRunner {
            clock,
            calls: Mutex::new(Calls {
                next_pid: 1000,
                ..Calls::default()
            }),
            processes: FakeProcessInspector::new(),
        }
    }

    /// Scripts the job `job`.
    pub fn script(&self, job: JobId, script: JobScript) {
        self.calls.lock().unwrap().scripts.insert(job, script);
    }

    /// Makes every later `spawn` of `command` fail, as a program that cannot be started.
    pub fn refuse_spawn(&self, command: &str) {
        self.calls.lock().unwrap().refused.push(command.to_string());
    }

    /// Every job spec passed to `run_job`, in call order.
    pub fn runs(&self) -> Vec<JobSpec> {
        self.calls.lock().unwrap().runs.clone()
    }

    /// Every spawn spec passed to `spawn`.
    pub fn spawns(&self) -> Vec<SpawnSpec> {
        self.calls.lock().unwrap().spawns.clone()
    }

    /// Every process passed to `kill_tree`.
    pub fn kills(&self) -> Vec<ProcessRef> {
        self.calls.lock().unwrap().kills.clone()
    }

    /// The process table spawned processes live in.
    pub fn processes(&self) -> FakeProcessInspector {
        self.processes.clone()
    }

    async fn wait(&self, limit: Duration, stop: &mut StopSignal, level: Stop) -> Waited {
        let mut sleep = self.clock.sleep(limit);
        let mut stopped = pin!(stop.reached(level));
        std::future::poll_fn(|cx| {
            if let Poll::Ready(s) = stopped.as_mut().poll(cx) {
                return Poll::Ready(Waited::Stopped(s));
            }
            sleep.as_mut().poll(cx).map(|_| Waited::Elapsed)
        })
        .await
    }

    async fn run_step(
        &self,
        script: &StepScript,
        limit: Duration,
        grace: Duration,
        stop: &mut StopSignal,
    ) -> (StepState, Option<i32>) {
        let runs_for = script.duration.min(limit);
        let begun = self.clock.now();
        match self.wait(runs_for, stop, Stop::Cancel).await {
            Waited::Elapsed if script.duration <= limit => {
                let state = if script.exit_code == 0 {
                    StepState::Passed
                } else {
                    StepState::Failed
                };
                (state, Some(script.exit_code))
            }
            Waited::Elapsed => (StepState::Timeout, None),
            Waited::Stopped(Stop::Kill) => (StepState::Cancelled, None),
            Waited::Stopped(Stop::Cancel) if script.obeys_soft_stop => (StepState::Cancelled, None),
            Waited::Stopped(Stop::Cancel) => {
                let spent = Duration::from_millis(self.clock.now().millis_since(begun));
                let left = runs_for.saturating_sub(spent).min(grace);
                self.wait(left, stop, Stop::Kill).await;
                (StepState::Cancelled, None)
            }
        }
    }
}

#[async_trait]
impl ProcessRunner for FakeProcessRunner {
    async fn run_job(
        &self,
        spec: JobSpec,
        output: &dyn JobOutput,
        mut stop: StopSignal,
    ) -> JobOutcome {
        let script = {
            let mut calls = self.calls.lock().unwrap();
            calls.runs.push(spec.clone());
            calls.scripts.get(&spec.job).cloned().unwrap_or_default()
        };
        let secrets = spec.secret_values();
        let budget_end = self.clock.now().plus_secs(spec.timeout_s);
        let mut state = JobState::Passed;
        let mut exit_code = None;
        let mut halted = false;
        let mut steps = Vec::with_capacity(spec.steps.len());
        for (index, step) in spec.steps.iter().enumerate() {
            let mut outcome = StepOutcome {
                name: step.spec.name.clone(),
                state: StepState::Skipped,
                exit_code: None,
                started_at: None,
                finished_at: None,
            };
            if !halted && stop.current().is_some() {
                state = JobState::Cancelled;
                halted = true;
            }
            let now = self.clock.now();
            if !halted && now >= budget_end {
                outcome.state = StepState::Timeout;
                state = JobState::Timeout;
                halted = true;
            }
            if halted {
                steps.push(outcome);
                continue;
            }
            let limit = Duration::from_millis(budget_end.millis_since(now))
                .min(Duration::from_secs(step.spec.timeout_s));
            let step_script = script
                .steps
                .get(index)
                .cloned()
                .unwrap_or_else(|| StepScript::new(Duration::ZERO, 0));
            output
                .emit(JobEvent::StepStarted {
                    index,
                    name: step.spec.name.clone(),
                    at: now,
                })
                .await;
            let process = ProcessRef {
                pid: FAKE_STEP_PID + u32::try_from(index).unwrap_or(0),
                start: Some(1),
            };
            output.emit(JobEvent::StepProcess { index, process }).await;
            if !step_script.output.is_empty() {
                let data = mask_secrets(&step_script.output, &secrets);
                output.emit(JobEvent::Output { index, data }).await;
            }
            let (step_state, code) = self
                .run_step(&step_script, limit, spec.cancel_grace, &mut stop)
                .await;
            outcome.state = step_state;
            outcome.exit_code = code;
            outcome.started_at = Some(now);
            outcome.finished_at = Some(self.clock.now());
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
            output
                .emit(JobEvent::StepFinished {
                    index,
                    outcome: outcome.clone(),
                })
                .await;
            steps.push(outcome);
        }
        if script.exceeded.is_some() && state == JobState::Passed {
            state = JobState::Failed;
        }
        JobOutcome {
            state,
            exit_code,
            steps,
            usage: script.usage,
            exceeded: script.exceeded.clone(),
        }
    }

    async fn spawn(&self, spec: SpawnSpec) -> Result<ProcessRef, ProcessError> {
        let process = {
            let mut calls = self.calls.lock().unwrap();
            if calls.refused.contains(&spec.command) {
                calls.spawns.push(spec);
                return Err(ProcessError::Spawn("program not found".to_string()));
            }
            calls.next_pid += 1;
            calls.spawns.push(spec);
            ProcessRef {
                pid: calls.next_pid,
                start: Some(self.clock.now().0),
            }
        };
        self.processes.add(process.clone());
        Ok(process)
    }

    async fn kill_tree(&self, process: &ProcessRef) -> Result<(), ProcessError> {
        let start = process
            .start
            .ok_or(ProcessError::Unverifiable(process.pid))?;
        self.calls.lock().unwrap().kills.push(process.clone());
        if self.processes.liveness(process.pid, start) == Liveness::Alive {
            self.processes.remove(process.pid);
        }
        Ok(())
    }
}

/// A [`JobOutput`] that keeps every event.
#[derive(Default)]
pub struct RecordingOutput {
    events: Mutex<Vec<JobEvent>>,
}

impl RecordingOutput {
    /// An empty recorder.
    pub fn new() -> RecordingOutput {
        RecordingOutput::default()
    }

    /// Events so far, in order.
    pub fn events(&self) -> Vec<JobEvent> {
        self.events.lock().unwrap().clone()
    }

    /// All output bytes so far, as lossy UTF-8.
    pub fn text(&self) -> String {
        let bytes: Vec<u8> = self
            .events()
            .into_iter()
            .filter_map(|e| match e {
                JobEvent::Output { data, .. } => Some(data),
                _ => None,
            })
            .flatten()
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

#[async_trait]
impl JobOutput for RecordingOutput {
    async fn emit(&self, event: JobEvent) {
        self.events.lock().unwrap().push(event);
    }
}
