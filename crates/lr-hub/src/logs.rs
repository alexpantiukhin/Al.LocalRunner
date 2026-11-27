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

const TAIL_BYTES: usize = 2048;

pub(crate) fn keep_tail(tail: &mut String, chunk: &str) {
    tail.push_str(chunk);
    if tail.len() > TAIL_BYTES {
        let mut cut = tail.len() - TAIL_BYTES;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail.drain(..cut);
    }
}

/// The line that follows a step that ended with exit code 127, the shell's "command not found":
/// the runner did not have a program. It names the program when the output of the step says
/// which (`sh: 1: git: not found`, `bash: git: command not found`) and says how to declare it.
pub(crate) fn not_found_hint(runner: &str, exit_code: Option<i32>, output: &str) -> Option<String> {
    if exit_code != Some(127) {
        return None;
    }
    let program = output.lines().rev().find_map(|line| {
        let head = line
            .trim()
            .strip_suffix(": command not found")
            .or_else(|| line.trim().strip_suffix(": not found"))?;
        head.rsplit([' ', ':']).next().filter(|p| !p.is_empty())
    });
    let what = program.map_or_else(|| "a program".to_string(), |p| format!("`{p}`"));
    let listed = program.map_or_else(|| "it".to_string(), |p| format!("`{p}`"));
    Some(format!(
        "=== hint: exit 127: {what} was not found on runner {runner}; if the profile needs it, list {listed} in `programs` of the profile so the order goes to a runner that has it ===\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_127_names_the_program_the_shell_did_not_find_and_how_to_declare_it() {
        let hint =
            not_found_hint("linux-heavy", Some(127), "/bin/sh: 1: git: not found\n").unwrap();
        assert!(
            hint.contains("`git` was not found on runner linux-heavy"),
            "{hint}"
        );
        assert!(hint.contains("list `git` in `programs`"), "{hint}");
        let bash =
            not_found_hint("r", Some(127), "bash: line 1: cargo: command not found").unwrap();
        assert!(bash.contains("`cargo`"), "{bash}");
        let unnamed = not_found_hint("r", Some(127), "something else\n").unwrap();
        assert!(
            unnamed.contains("a program was not found on runner r"),
            "{unnamed}"
        );
    }

    #[test]
    fn other_exit_codes_get_no_hint() {
        assert!(not_found_hint("r", Some(1), "git: not found").is_none());
        assert!(not_found_hint("r", None, "git: not found").is_none());
    }

    #[test]
    fn the_tail_keeps_the_last_bytes_on_a_character_boundary() {
        let mut tail = String::new();
        keep_tail(&mut tail, &"é".repeat(3000));
        assert!(tail.len() <= TAIL_BYTES + 1);
        assert!(tail.chars().all(|c| c == 'é'));
    }
}
