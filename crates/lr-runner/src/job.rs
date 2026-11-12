use crate::agent::{Current, Shared};
use crate::connect::Session;
use crate::hooks_config::CleanupOn;
use crate::prepare::{apply_job_temp, job_env, job_temp, resolve_commands, step_secrets, Prepared};
use crate::run::run_phases;
use lr_abstractions::{stop_signal, Level, Stop, StoreError};
use lr_core::{JobId, JobState, StepState};
use lr_proto::{
    Assign, JobResult, ReasonCode, RejectReason, RunnerCancel, RunnerToBroker, Started, StepResult,
};
use std::sync::Arc;

pub(crate) struct Final {
    pub(crate) state: JobState,
    pub(crate) exit_code: Option<i32>,
    pub(crate) reason: Option<ReasonCode>,
    pub(crate) detail: Option<String>,
    pub(crate) peak_mem_mb: Option<u64>,
    pub(crate) peak_disk_mb: Option<u64>,
}

impl Final {
    pub(crate) fn failed(detail: impl Into<String>) -> Final {
        Final {
            state: JobState::Failed,
            exit_code: None,
            reason: None,
            detail: Some(detail.into()),
            peak_mem_mb: None,
            peak_disk_mb: None,
        }
    }
}

async fn known(shared: &Shared, job: &JobId) -> Result<bool, StoreError> {
    if shared.state().job.as_ref().is_some_and(|c| &c.job == job) {
        return Ok(true);
    }
    if shared.deps.store.last_seq(job).await? > 0 {
        return Ok(true);
    }
    Ok(shared
        .deps
        .store
        .pending_outbox()
        .await?
        .iter()
        .any(|e| &e.job == job))
}

enum Seq {
    New,
    Repeat,
    Dropped,
}

async fn accept(s: &Session<'_>, job: &JobId, seq: u64) -> Seq {
    match s.shared.deps.store.accept_seq(job, seq).await {
        Ok(true) => Seq::New,
        Ok(false) => {
            s.ack(job, seq).await;
            Seq::Repeat
        }
        Err(StoreError::SeqGap { expected, got }) => {
            s.refuse_gap(job, expected, got).await;
            Seq::Dropped
        }
        Err(e) => {
            s.shared.diag.event(
                Level::Error,
                "received seq not stored, message dropped",
                &[("job", &job.0), ("error", &e.to_string())],
            );
            Seq::Dropped
        }
    }
}

fn result_msg(job: &JobId, f: Final) -> RunnerToBroker {
    RunnerToBroker::JobResult(JobResult {
        job: job.clone(),
        seq: 0,
        state: f.state,
        exit_code: f.exit_code,
        reason: f.reason,
        detail: f.detail,
        peak_disk_mb: f.peak_disk_mb,
        peak_mem_mb: f.peak_mem_mb,
    })
}

pub(crate) async fn finish(shared: &Shared, job: &JobId, f: Final) {
    let state = format!("{:?}", f.state).to_lowercase();
    let reason = f
        .reason
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    shared.diag.event(
        Level::Info,
        "job finished",
        &[("job", &job.0), ("state", &state), ("reason", &reason)],
    );
    shared
        .outbox
        .push_until_stored(job, &result_msg(job, f))
        .await;
}

async fn refuse(s: &Session<'_>, a: &Assign, f: Final) {
    let why = f.detail.clone().unwrap_or_default();
    s.shared.diag.event(
        Level::Warn,
        "assignment refused",
        &[("job", &a.job.0), ("why", &why)],
    );
    s.shared
        .outbox
        .push_once(
            &a.job,
            &result_msg(&a.job, f),
            "job result not stored, dropped",
        )
        .await;
    s.ack(&a.job, a.seq).await;
}

pub(crate) async fn skip_steps(
    shared: &Shared,
    job: &JobId,
    steps: &[lr_core::StepSpec],
    from: usize,
) {
    for (ord, step) in steps.iter().enumerate().skip(from) {
        let msg = RunnerToBroker::StepResult(StepResult {
            job: job.clone(),
            seq: 0,
            ord: u32::try_from(ord).unwrap_or(u32::MAX),
            name: step.name.clone(),
            state: StepState::Skipped,
            exit_code: None,
            duration_ms: 0,
        });
        shared.outbox.push_or_log(job, &msg).await;
    }
}

pub(crate) async fn on_assign(s: &mut Session<'_>, mut a: Assign) {
    let shared = s.shared;
    let was_known = match known(shared, &a.job).await {
        Ok(k) => k,
        Err(e) => {
            shared.diag.event(
                Level::Error,
                "runner store unreadable, assignment dropped",
                &[("job", &a.job.0), ("error", &e.to_string())],
            );
            return;
        }
    };
    match accept(s, &a.job, a.seq).await {
        Seq::New => {}
        Seq::Repeat | Seq::Dropped => return,
    }
    shared.diag.event(
        Level::Info,
        "job received",
        &[("job", &a.job.0), ("client", &a.spec.client)],
    );
    if was_known {
        s.ack(&a.job, a.seq).await;
        shared.diag.event(
            Level::Info,
            "job already known, not run again",
            &[("job", &a.job.0)],
        );
        shared.wake.notify_one();
        return;
    }
    let busy = shared.state().job.as_ref().map(|c| c.job.clone());
    let refusal = if shared.state().draining {
        Some("runner is draining".to_string())
    } else if let Some(other) = busy {
        Some(format!("runner is busy with job {other}"))
    } else {
        let t = a.spec.requirements.timeout_s;
        shared
            .deps
            .settings
            .max_job_timeout_s
            .filter(|ceiling| t > *ceiling)
            .map(|ceiling| format!("job timeout {t} s is above the runner ceiling {ceiling} s"))
            .or_else(|| {
                shared.deps.settings.path.is_none().then(|| {
                    "runner.toml has no [env] path, so the job would get no PATH; set [env] path (spec 13.4)"
                        .to_string()
                })
            })
    };
    if let Some(detail) = refusal {
        return refuse(s, &a, Final::failed(detail)).await;
    }
    if let Err(detail) = resolve_commands(&shared.deps.settings.commands, &mut a.spec) {
        let f = Final {
            reason: Some(ReasonCode::Rejected(RejectReason::CommandNotAllowed)),
            ..Final::failed(detail)
        };
        skip_steps(shared, &a.job, &a.spec.steps, 0).await;
        return refuse(s, &a, f).await;
    }
    if let Err(e) = shared
        .deps
        .files
        .write(
            &shared.marker(),
            &crate::background::marker_text(&a.job, None),
        )
        .await
    {
        let detail = format!("active job marker not written: {e}");
        shared.diag.event(
            Level::Error,
            "active job marker not written, job not run",
            &[("job", &a.job.0), ("error", &e.to_string())],
        );
        skip_steps(shared, &a.job, &a.spec.steps, 0).await;
        return refuse(s, &a, Final::failed(detail)).await;
    }
    let (handle, signal) = stop_signal();
    shared.state().job = Some(Current {
        job: a.job.clone(),
        stop: handle,
        reason: None,
        lost_killed: false,
    });
    shared.status.notify_one();
    s.ack(&a.job, a.seq).await;
    let task_shared: Arc<Shared> = s.shared.clone();
    s.slot.spawn(async move {
        execute(&task_shared, a, signal).await;
    });
}

pub(crate) async fn on_cancel(s: &mut Session<'_>, c: RunnerCancel) {
    match accept(s, &c.job, c.seq).await {
        Seq::New => {
            s.ack(&c.job, c.seq).await;
        }
        Seq::Repeat | Seq::Dropped => return,
    }
    let running = s
        .shared
        .state()
        .job
        .as_ref()
        .is_some_and(|cur| cur.job == c.job);
    if !running {
        s.shared.diag.event(
            Level::Info,
            "cancel of a job not running",
            &[("job", &c.job.0)],
        );
        return;
    }
    let text = match &c.detail {
        Some(d) => format!("cancelled ({}): {d}", c.code),
        None => format!("cancelled ({})", c.code),
    };
    s.shared.stop_job(Stop::Cancel, c.code, &text);
}

async fn execute(shared: &Shared, a: Assign, signal: lr_abstractions::StopSignal) {
    let job = a.job.clone();
    let marker = shared.marker();
    let started = RunnerToBroker::Started(Started {
        job: job.clone(),
        seq: 0,
    });
    shared.outbox.push_until_stored(&job, &started).await;
    if let Err(e) = shared.deps.awake.hold() {
        shared.diag.event(
            Level::Warn,
            "keep-awake not held",
            &[("job", &job.0), ("error", &e)],
        );
    }
    let mut held = Vec::new();
    let f = match step_secrets(shared, &a.spec).await {
        Err(f) => {
            skip_steps(shared, &job, &a.spec.steps, 0).await;
            f
        }
        Ok(secrets) => {
            let (acquired, ready) = shared.acquire_services(&a.spec, &signal).await;
            held = acquired;
            match ready {
                Err(f) => {
                    skip_steps(shared, &job, &a.spec.steps, 0).await;
                    f
                }
                Ok(()) => {
                    let mut env = job_env(shared, &job);
                    apply_job_temp(&mut env, &job_temp(shared, &job));
                    let prepared = Prepared {
                        job: job.clone(),
                        spec: &a.spec,
                        secrets,
                        env,
                    };
                    run_phases(shared, prepared, signal).await
                }
            }
        }
    };
    shared.release_services(&held).await;
    let reason = shared.state().job.as_ref().and_then(|c| c.reason.clone());
    let f = Final {
        reason: f.reason.or(if f.state == JobState::Cancelled {
            reason
        } else {
            None
        }),
        ..f
    };
    crate::hooks::run_cleanup(shared, &job, CleanupOn::of(f.state, f.reason.as_ref())).await;
    finish(shared, &job, f).await;
    shared.release_disk().await;
    shared.deps.awake.release();
    if let Err(e) = shared.deps.files.remove_file(&marker).await {
        shared.diag.event(
            Level::Error,
            "active job marker not removed",
            &[("job", &job.0), ("error", &e.to_string())],
        );
    }
    {
        let mut st = shared.state();
        st.job = None;
        st.in_use.clear();
        st.caps_dirty = true;
    }
    shared.forget_finished().await;
    shared.wake.notify_one();
}
