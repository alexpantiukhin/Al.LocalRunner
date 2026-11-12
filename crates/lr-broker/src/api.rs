//! Read API of the broker: who sees what of a run (spec 3.2 `[visibility] others_orders`) and
//! the views built from the stored rows (spec 9.1).

use crate::engine::Job;
use crate::policy::OthersOrders;
use lr_abstractions::{JobRecord, JobRow, SourceKind, Spread, Stats, StepRow};
use lr_core::{JobState, StepState, Timestamp};
use lr_proto::{
    ClientUsageView, JobDetail, JobSummary, ProfileStatsView, QueueEvent, ReasonCode, SpreadView,
    StatsView, StepStatsView, StepView, Topic, WireSource,
};

/// How much of a run a reader may see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Access {
    /// Not even that it exists (`others_orders = "none"`); only the pool load counts it.
    Hidden,
    /// Identifier, state, platform, runner, times and client label (`summary`).
    Summary,
    /// Every order field, never the output (`full`).
    Full,
    /// Everything including the log: the run's own client or the broker owner.
    Owner,
}

impl Access {
    /// Whether the reader gets the job output (log, last lines).
    pub fn sees_log(self) -> bool {
        self == Access::Owner
    }

    /// Whether the reader sees the order fields beyond the summary.
    pub fn sees_fields(self) -> bool {
        self >= Access::Full
    }
}

/// Who reads: the client key of the connection and whether it is the broker owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    /// Client key.
    pub key: String,
    /// The broker owner sees everything.
    pub owner: bool,
}

/// What `viewer` may see of a run of client `run_client` under `others`.
pub fn access(viewer: &Viewer, run_client: &str, others: OthersOrders) -> Access {
    if viewer.owner || viewer.key == run_client {
        return Access::Owner;
    }
    match others {
        OthersOrders::Hidden => Access::Hidden,
        OthersOrders::Summary => Access::Summary,
        OthersOrders::Full => Access::Full,
    }
}

/// The summary as `access` allows: `None` when hidden; project, label, profile, source, note,
/// branch and directory removed at `summary`.
pub fn mask_summary(mut s: JobSummary, access: Access) -> Option<JobSummary> {
    match access {
        Access::Hidden => None,
        Access::Summary => {
            s.project = None;
            s.label = None;
            s.profile = None;
            s.source = None;
            s.note = None;
            s.branch = None;
            s.cwd = None;
            s.timeout_s = None;
            s.github_run_url = None;
            s.peak_mem_mb = None;
            s.peak_disk_mb = None;
            Some(s)
        }
        Access::Full | Access::Owner => Some(s),
    }
}

/// The full record as `access` allows: at `summary` only the summary fields remain.
pub fn mask_detail(mut d: JobDetail, access: Access) -> Option<JobDetail> {
    d.summary = mask_summary(d.summary, access)?;
    if !access.sees_fields() {
        d.note = None;
        d.branch = None;
        d.cwd = None;
        d.commit = None;
        d.os_user = None;
        d.source = None;
        d.github_run_url = None;
        d.steps.clear();
        d.secrets.clear();
        d.cancelled_by = None;
        d.wait_reason = None;
    }
    Some(d)
}

/// An event as `access` allows, `None` when the reader may not see it. Runner events are pool
/// occupancy and always visible.
pub fn mask_event(event: QueueEvent, access: Access) -> Option<QueueEvent> {
    match event {
        QueueEvent::JobQueued { job } => {
            mask_summary(*job, access).map(|job| QueueEvent::JobQueued { job: Box::new(job) })
        }
        QueueEvent::StepChanged { .. } => access.sees_fields().then_some(event),
        QueueEvent::JobStarted { .. } | QueueEvent::JobFinished { .. } => {
            (access != Access::Hidden).then_some(event)
        }
        other => Some(other),
    }
}

/// The subscription topic of an event.
pub fn topic_of(event: &QueueEvent) -> Topic {
    match event {
        QueueEvent::JobQueued { .. }
        | QueueEvent::JobStarted { .. }
        | QueueEvent::StepChanged { .. }
        | QueueEvent::JobFinished { .. } => Topic::Queue,
        QueueEvent::RunnerOnline { .. }
        | QueueEvent::RunnerOffline { .. }
        | QueueEvent::ServiceChanged { .. } => Topic::Runners,
        QueueEvent::Unknown => Topic::Unknown,
    }
}

/// The reason code a stored `cancel_reason` starts with (`code` or `code: text`).
pub fn reason_of(cancel_reason: Option<&str>) -> Option<ReasonCode> {
    let text = cancel_reason?;
    let code = text.split(": ").next().unwrap_or(text);
    code.parse().ok()
}

/// The explanation stored with a rejected order (`rejected_*: text`).
fn rejection_text(row: &JobRow) -> Option<String> {
    let text = row.cancel_reason.as_deref()?;
    let Some(ReasonCode::Rejected(_)) = reason_of(Some(text)) else {
        return None;
    };
    text.split_once(": ").map(|(_, d)| d.to_string())
}

/// The sentence about the peaks of a finished run, `None` when the runner reported none.
pub fn peaks_text(row: &JobRow) -> Option<String> {
    let mb = |v: Option<u64>| v.map_or("-".to_string(), |v| format!("{v} МБ"));
    (row.peak_mem_mb.is_some() || row.peak_disk_mb.is_some()).then(|| {
        format!(
            "пик памяти {}, пик диска {}",
            mb(row.peak_mem_mb),
            mb(row.peak_disk_mb)
        )
    })
}

/// The wire form of a source kind.
pub fn wire_source(s: SourceKind) -> WireSource {
    match s {
        SourceKind::Session => WireSource::Session,
        SourceKind::Github => WireSource::Github,
    }
}

/// The unmasked summary of a stored run.
pub fn summary_of(row: &JobRow) -> JobSummary {
    JobSummary {
        job: row.key.job.clone(),
        client_label: row.client_label.clone(),
        state: row.state,
        platform: row.platform.clone(),
        runner: row.runner.clone(),
        queued_at: row.queued_at,
        started_at: row.started_at,
        finished_at: row.finished_at,
        exit_code: row.exit_code,
        reason: reason_of(row.cancel_reason.as_deref()),
        project: Some(row.project.clone()),
        label: Some(row.label.clone()),
        profile: Some(row.profile.clone()),
        source: Some(wire_source(row.source)),
        note: row.note.clone(),
        branch: row.branch.clone(),
        cwd: Some(row.cwd.clone()),
        timeout_s: None,
        github_run_url: None,
        peak_mem_mb: row.peak_mem_mb,
        peak_disk_mb: row.peak_disk_mb,
    }
}

/// The unmasked summary of a stored run with what its other rows add: the job timeout from
/// the resolved order and the link to the GitHub run.
pub fn record_summary(rec: &JobRecord) -> JobSummary {
    let mut s = summary_of(&rec.job);
    s.github_run_url = rec.github.as_ref().map(|g| g.run_url.clone());
    s.timeout_s = serde_json::from_str::<serde_json::Value>(&rec.spec.resolved_json)
        .ok()
        .and_then(|v| {
            v.pointer("/requirements/timeout_s")
                .and_then(|t| t.as_u64())
        });
    s
}

/// The view of a stored step.
pub fn step_view(row: &StepRow) -> StepView {
    StepView {
        ord: row.ord,
        name: row.name.clone(),
        state: row.state,
        exit_code: row.exit_code,
        started_at: row.started_at,
        finished_at: row.finished_at,
        timeout_s: row.timeout_s,
    }
}

/// Steps as they stand while the job runs: the first queued step after only passed ones is
/// the one executing, started when the step before it ended, the
/// first one when the job started. The runner reports a step only once it ends. Once the
/// job is final, a step still queued never ran: it is shown skipped.
pub fn live_steps(
    mut steps: Vec<StepView>,
    state: JobState,
    started: Option<Timestamp>,
) -> Vec<StepView> {
    if state.is_final() {
        for s in steps.iter_mut().filter(|s| s.state == StepState::Queued) {
            s.state = StepState::Skipped;
        }
        return steps;
    }
    if !matches!(state, JobState::Running | JobState::Cancelling) || started.is_none() {
        return steps;
    }
    if steps.iter().any(|s| s.state == StepState::Running) {
        return steps;
    }
    let Some(i) = steps.iter().position(|s| s.state == StepState::Queued) else {
        return steps;
    };
    let before = &steps[..i];
    let blocked = before.iter().any(|s| {
        matches!(
            s.state,
            StepState::Failed | StepState::Timeout | StepState::Skipped | StepState::Cancelled
        )
    });
    if !blocked {
        let since = before.last().map_or(started, |s| s.finished_at.or(started));
        steps[i].state = StepState::Running;
        steps[i].started_at = since;
    }
    steps
}

/// The unmasked full record of a stored run.
pub fn detail_of(rec: &JobRecord, wait_reason: Option<String>) -> JobDetail {
    let row = &rec.job;
    let wait_reason = wait_reason.or_else(|| rejection_text(row));
    let summary = record_summary(rec);
    JobDetail {
        summary,
        note: row.note.clone(),
        branch: row.branch.clone(),
        cwd: Some(row.cwd.clone()),
        commit: row.commit_sha.clone(),
        os_user: Some(row.os_user.clone()),
        source: Some(wire_source(row.source)),
        github_run_url: rec.github.as_ref().map(|g| g.run_url.clone()),
        steps: live_steps(
            rec.steps.iter().map(step_view).collect(),
            row.state,
            row.started_at,
        ),
        secrets: row.secrets.clone(),
        cancelled_by: row.cancelled_by.clone(),
        wait_reason,
    }
}

pub(crate) fn summary_of_job(job: &Job) -> JobSummary {
    let state = match (job.started_at, &job.cancel) {
        (None, _) => JobState::Queued,
        (Some(_), None) => JobState::Running,
        (Some(_), Some(_)) => JobState::Cancelling,
    };
    JobSummary {
        job: job.key.job.clone(),
        client_label: job.entry.client.label.clone(),
        state,
        platform: job.entry.order.requirements.platform.clone(),
        runner: job.runner.clone(),
        queued_at: job.spec.enqueued_at,
        started_at: job.started_at,
        finished_at: None,
        exit_code: None,
        reason: None,
        project: Some(job.spec.identity.project.clone()),
        label: Some(job.spec.identity.label.clone()),
        profile: Some(job.spec.profile.clone()),
        source: Some(match job.spec.identity.source {
            lr_core::Source::Session => WireSource::Session,
            lr_core::Source::Github(_) => WireSource::Github,
        }),
        note: job.spec.identity.note.clone(),
        branch: job.spec.identity.branch.clone(),
        cwd: Some(job.spec.identity.cwd.display().to_string()),
        timeout_s: Some(job.entry.order.requirements.timeout_s),
        github_run_url: match &job.spec.identity.source {
            lr_core::Source::Github(g) => Some(g.run_url.clone()),
            lr_core::Source::Session => None,
        },
        peak_mem_mb: None,
        peak_disk_mb: None,
    }
}

fn spread(s: &Spread) -> SpreadView {
    SpreadView {
        median: s.median,
        p95: s.p95,
    }
}

/// The wire form of the statistics.
pub fn stats_view(s: &Stats) -> StatsView {
    StatsView {
        profiles: s
            .profiles
            .iter()
            .map(|p| ProfileStatsView {
                profile: p.profile.clone(),
                runs: p.runs,
                passed: p.passed,
                failed: p.failed,
                timeouts: p.timeouts,
                cancelled: p.cancelled,
                lost: p.lost,
                wait_ms: spread(&p.wait_ms),
                duration_ms: spread(&p.duration_ms),
                steps: p
                    .steps
                    .iter()
                    .map(|st| StepStatsView {
                        name: st.name.clone(),
                        runs: st.runs,
                        passed: st.passed,
                        duration_ms: spread(&st.duration_ms),
                    })
                    .collect(),
            })
            .collect(),
        clients: s
            .clients
            .iter()
            .map(|c| ClientUsageView {
                client: c.client.clone(),
                label: c.label.clone(),
                runs: c.runs,
                busy_ms: c.busy_ms,
            })
            .collect(),
    }
}
