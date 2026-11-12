use crate::agent::{after, Shared, SWEEP_EVERY};
use crate::service_stop::service_reaper;
use lr_abstractions::metric::OUTBOX_SAMPLE_S;
use lr_abstractions::{Level, OutboxKind, ProcessRef, Stop};
use lr_core::{JobId, JobState, Timestamp};
use lr_proto::{JobResult, ReasonCode, RunnerToBroker};
use std::sync::Arc;
use std::time::Duration;

pub(crate) async fn background<T>(shared: &Arc<Shared>, next_sweep: &mut Timestamp) -> T {
    tokio::select! {
        never = sweeper(shared, next_sweep) => never,
        never = lost_killer(shared) => never,
        never = outbox_sampler(shared) => never,
        never = service_reaper(shared) => never,
    }
}

async fn sweeper<T>(shared: &Shared, next: &mut Timestamp) -> T {
    loop {
        shared.deps.clock.sleep_until(*next).await;
        *next = after(*next, SWEEP_EVERY);
        shared.sweep().await;
    }
}

async fn outbox_sampler<T>(shared: &Shared) -> T {
    loop {
        let at = shared.state().next_outbox_sample;
        shared.deps.clock.sleep_until(at).await;
        shared.state().next_outbox_sample = shared.now().plus_secs(OUTBOX_SAMPLE_S);
        shared.sample_outbox().await;
    }
}

fn kill_at(shared: &Shared) -> Option<Timestamp> {
    let st = shared.state();
    match (&st.job, st.lost_since) {
        (Some(cur), Some(since)) if !cur.lost_killed => {
            let grace = shared.deps.settings.lost_kill_s(st.broker_lost_kill_s);
            Some(after(since, Duration::from_secs(grace)))
        }
        _ => None,
    }
}

async fn lost_killer<T>(shared: &Shared) -> T {
    loop {
        match kill_at(shared) {
            Some(at) if shared.now() >= at => {
                if let Some(cur) = shared.state().job.as_mut() {
                    cur.lost_killed = true;
                }
                shared.diag.event(
                    Level::Warn,
                    "broker unreachable past the grace, job killed",
                    &[],
                );
                shared.stop_job(
                    Stop::Kill,
                    ReasonCode::BrokerUnavailable,
                    "broker unreachable past the grace",
                );
            }
            Some(at) => tokio::select! {
                _ = shared.deps.clock.sleep_until(at) => {}
                _ = shared.status.notified() => {}
            },
            None => shared.status.notified().await,
        }
    }
}

/// The active job marker: the job id, then `pid start` of the root of its running step.
pub(crate) fn marker_text(job: &JobId, process: Option<&ProcessRef>) -> Vec<u8> {
    let mut text = job.0.clone();
    if let Some(ProcessRef {
        pid,
        start: Some(start),
    }) = process
    {
        text.push_str(&format!("\n{pid} {start}"));
    }
    text.into_bytes()
}

fn parse_marker(bytes: &[u8]) -> (JobId, Option<ProcessRef>) {
    let text = String::from_utf8_lossy(bytes);
    let mut lines = text.lines();
    let job = JobId(lines.next().unwrap_or_default().trim().to_string());
    let process = lines.next().and_then(|l| {
        let (pid, start) = l.trim().split_once(' ')?;
        Some(ProcessRef {
            pid: pid.parse().ok()?,
            start: Some(start.parse().ok()?),
        })
    });
    (job, process)
}

async fn kill_left(shared: &Shared, job: &JobId, process: &ProcessRef) {
    if let Err(e) = shared.deps.process.kill_tree(process).await {
        shared.diag.event(
            Level::Error,
            "processes of the job from before the restart not killed",
            &[("job", &job.0), ("error", &e.to_string())],
        );
    }
}

pub(crate) async fn recover(shared: &Shared) {
    let marker = shared.marker();
    shared.deps.process.sweep_orphans().await;
    match shared.deps.files.read(&marker).await {
        Ok(Some(bytes)) => {
            let (job, process) = parse_marker(&bytes);
            if let Some(process) = &process {
                kill_left(shared, &job, process).await;
            }
            restarted(shared, job).await;
            if let Err(e) = shared.deps.files.remove_file(&marker).await {
                shared.diag.event(
                    Level::Error,
                    "active job marker not removed",
                    &[("error", &e.to_string())],
                );
            }
        }
        Ok(None) => {}
        Err(e) => shared.diag.event(
            Level::Error,
            "active job marker unreadable",
            &[("error", &e.to_string())],
        ),
    }
    shared.release_disk().await;
    shared.forget_undeclared_services().await;
    shared.services().kill_orphans().await;
    shared.forget_finished().await;
    shared.sweep().await;
}

async fn restarted(shared: &Shared, job: JobId) {
    let pending = match shared.deps.store.pending_outbox().await {
        Ok(p) => p,
        Err(e) => {
            shared.diag.event(
                Level::Error,
                "outbox unreadable at start",
                &[("error", &e.to_string())],
            );
            Vec::new()
        }
    };
    if pending
        .iter()
        .any(|e| e.job == job && e.kind == OutboxKind::JobResult)
    {
        return;
    }
    shared.diag.event(
        Level::Warn,
        "runner restarted while a job ran",
        &[("job", &job.0)],
    );
    let msg = RunnerToBroker::JobResult(JobResult {
        job: job.clone(),
        seq: 0,
        state: JobState::Failed,
        exit_code: None,
        reason: Some(ReasonCode::RunnerRestarted),
        detail: Some("the runner restarted while the job ran".into()),
    });
    shared.outbox.push_until_stored(&job, &msg).await;
}
