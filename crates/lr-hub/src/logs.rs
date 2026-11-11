use lr_core::{Identity, JobId, JobState, Source, Timestamp};
use lr_proto::{OrderSpec, ReasonCode, StepResult};

fn state_name(state: JobState) -> &'static str {
    match state {
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::Cancelling => "cancelling",
        JobState::Passed => "passed",
        JobState::Failed => "failed",
        JobState::Timeout => "timeout",
        JobState::Cancelled => "cancelled",
        JobState::Lost => "lost",
    }
}

fn source_name(id: &Identity) -> &'static str {
    match id.source {
        Source::Session => "session",
        Source::Github(_) => "github",
    }
}

pub(crate) fn header(job: &JobId, spec: &OrderSpec) -> String {
    let id = &spec.identity;
    let mut text = format!(
        "=== localrunner job {job} · {} · {} · {} · profile {} ===\n",
        id.project,
        source_name(id),
        id.label,
        spec.profile
    );
    if let Some(note) = &id.note {
        text.push_str(&format!("=== note: {note} ===\n"));
    }
    if let Some(branch) = &id.branch {
        text.push_str(&format!("=== branch {branch} · {} ===\n", id.cwd.display()));
    }
    text
}

pub(crate) fn cancel_line(
    by: &str,
    at: Timestamp,
    force: bool,
    code: &ReasonCode,
    reason: Option<&str>,
) -> String {
    let mut text = format!("=== cancel requested by {by} at {at} ({code})");
    if force {
        text.push_str(" --force");
    }
    if let Some(r) = reason {
        text.push_str(&format!(": {r}"));
    }
    text.push_str(" ===\n");
    text
}

pub(crate) fn trimmed_line(asked: Option<u64>, to: u64) -> String {
    match asked {
        Some(asked) => {
            format!("=== timeout trimmed by the broker policy: {asked} s -> {to} s ===\n")
        }
        None => format!("=== timeout trimmed by the broker policy to {to} s ===\n"),
    }
}

pub(crate) fn started_line(runner: &str, at: Timestamp) -> String {
    format!("=== started on runner {runner} at {at} ===\n")
}

pub(crate) fn step_line(r: &StepResult) -> String {
    let code = r
        .exit_code
        .map_or_else(String::new, |c| format!(", exit {c}"));
    format!(
        "=== step {} {}{code}, {} ms ===\n",
        r.name,
        match r.state {
            lr_core::StepState::Queued => "queued",
            lr_core::StepState::Running => "running",
            lr_core::StepState::Passed => "passed",
            lr_core::StepState::Failed => "failed",
            lr_core::StepState::Timeout => "timeout",
            lr_core::StepState::Skipped => "skipped",
            lr_core::StepState::Cancelled => "cancelled",
        },
        r.duration_ms
    )
}

pub(crate) fn result_line(
    state: JobState,
    exit_code: Option<i32>,
    reason: Option<&ReasonCode>,
) -> String {
    let mut text = format!("=== result: {}", state_name(state));
    if let Some(c) = exit_code {
        text.push_str(&format!(", exit {c}"));
    }
    if let Some(r) = reason {
        text.push_str(&format!(", {r}"));
    }
    text.push_str(" ===\n");
    text
}

pub(crate) fn rejected_line(code: &ReasonCode, detail: &str) -> String {
    format!("=== rejected: {code}: {detail} ===\n")
}
