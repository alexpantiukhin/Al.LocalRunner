use crate::agent::Shared;
use crate::hooks_config::{apply_owner_env, CleanupHook, CleanupOn};
use crate::output::Forward;
use crate::prepare::{job_env, job_label};
use lr_abstractions::{
    stop_signal, JobEvent, JobOutput, JobSpec, JobStep, Level, DEFAULT_CANCEL_GRACE,
};
use lr_core::{JobId, JobState, StepSpec};

fn hook_spec(
    shared: &Shared,
    job: &JobId,
    hook: &CleanupHook,
    vars: &[(String, String)],
) -> JobSpec {
    let step = StepSpec {
        name: format!("cleanup {}", hook.name),
        command: hook.command.clone(),
        timeout_s: hook.timeout_s,
        mem_mb: None,
        cpu: None,
        tags: None,
        secrets: Vec::new(),
        continue_on_failure: true,
    };
    JobSpec {
        job: JobId(format!("{}.cleanup.{}", job.0, hook.name)),
        steps: vec![JobStep::from(step)],
        timeout_s: hook.timeout_s,
        cwd: shared.deps.settings.state_dir.clone(),
        env: vars.to_vec(),
        cancel_grace: DEFAULT_CANCEL_GRACE,
        limits: Default::default(),
        header: vec![("cleanup".to_string(), hook.name.clone())],
    }
}

async fn run_hook(shared: &Shared, job: &JobId, hook: &CleanupHook, vars: &[(String, String)]) {
    let forward = Forward::new(shared, job.clone(), vec![None]);
    let (_handle, signal) = stop_signal();
    let spec = hook_spec(shared, job, hook, vars);
    let outcome = shared.deps.process.run_job(spec, &forward, signal).await;
    forward.report_rest(&outcome).await;
    if outcome.state == JobState::Passed {
        return;
    }
    let state = format!("{:?}", outcome.state).to_lowercase();
    let code = outcome
        .exit_code
        .map_or_else(String::new, |c| c.to_string());
    shared.diag.event(
        Level::Warn,
        "cleanup hook failed",
        &[
            ("job", &job.0),
            ("hook", &hook.name),
            ("state", &state),
            ("exit_code", &code),
        ],
    );
    let line = format!(
        "warning: cleanup {} ended {state}{}\n",
        hook.name,
        outcome
            .exit_code
            .map_or_else(String::new, |c| format!(" (exit code {c})"))
    );
    forward
        .emit(JobEvent::Output {
            index: 0,
            data: line.into_bytes(),
        })
        .await;
}

/// Runs the `[[cleanup]]` hooks that apply to `how` the job ended, one after another, each as
/// a short job of its own; a failing hook only logs a warning and never changes the result.
pub(crate) async fn run_cleanup(shared: &Shared, job: &JobId, how: CleanupOn) {
    let hooks = &shared.deps.settings.cleanup;
    if hooks.iter().all(|h| !h.on.contains(&how)) {
        return;
    }
    let mut vars = job_env(shared, job);
    if !vars.iter().any(|(n, _)| n == "LOCALRUNNER_JOB_LABEL") {
        vars.push(("LOCALRUNNER_JOB_LABEL".to_string(), job_label(job)));
    }
    apply_owner_env(&mut vars, &shared.deps.settings.job_env);
    for hook in hooks.iter().filter(|h| h.on.contains(&how)) {
        run_hook(shared, job, hook, &vars).await;
    }
}
