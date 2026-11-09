use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

const MAX_LABEL_CHARS: usize = 100;
const MAX_NOTE_CHARS: usize = 200;

/// Errors produced while collecting a submitter identity.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum IdentityError {
    /// The label is missing, empty or whitespace-only.
    #[error("a label (--as) is required")]
    MissingLabel,
    /// The label is longer than 100 characters.
    #[error("the label is longer than {MAX_LABEL_CHARS} characters")]
    LabelTooLong,
    /// The label contains control characters.
    #[error("the label contains control characters")]
    LabelInvalid,
    /// The note is longer than 200 characters.
    #[error("the note is longer than {MAX_NOTE_CHARS} characters")]
    NoteTooLong,
    /// `GITHUB_ACTIONS` is not `true`.
    #[error("not running inside GitHub Actions (GITHUB_ACTIONS is not `true`)")]
    NotGithubActions,
    /// A required GitHub Actions variable is missing or malformed.
    #[error("GitHub Actions variable {0} is missing or invalid")]
    MissingEnv(String),
}

/// Facts about a GitHub Actions run, read from its environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubInfo {
    /// `owner/repo` (`GITHUB_REPOSITORY`).
    pub repository: String,
    /// Workflow name (`GITHUB_WORKFLOW`).
    pub workflow: String,
    /// Job id (`GITHUB_JOB`).
    pub job: String,
    /// Numeric run id (`GITHUB_RUN_ID`).
    pub run_id: u64,
    /// Run attempt (`GITHUB_RUN_ATTEMPT`), absent when the variable is not set.
    pub attempt: Option<u64>,
    /// Git ref that triggered the run (`GITHUB_REF`), empty when unset.
    pub git_ref: String,
    /// Actor that triggered the run (`GITHUB_ACTOR`), empty when unset.
    pub actor: String,
    /// Link to the run page.
    pub run_url: String,
}

/// Where a job came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// A terminal or an agent session.
    Session,
    /// A GitHub Actions job.
    Github(GithubInfo),
}

/// Who submitted a job, collected by the CLI and not overridable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// Free-form label of the submitter.
    pub label: String,
    /// What the run is for, up to 200 characters.
    pub note: Option<String>,
    /// OS user name.
    pub os_user: String,
    /// Host name.
    pub host: String,
    /// Submitter process id: the parent of the CLI process, that is the shell or
    /// agent session that called it. `0` when the parent cannot be determined;
    /// such a submitter is never treated as dead.
    pub sender_pid: u32,
    /// Start of the submitter process in unix milliseconds, `None` when unobtainable.
    /// On Unix the value comes from the elapsed time reported by `ps` and is accurate to about a second.
    pub sender_start: Option<u64>,
    /// Working directory of the submitter.
    pub cwd: PathBuf,
    /// Current branch, `None` when detached, without commits or outside git.
    pub branch: Option<String>,
    /// Root of the worktree containing `cwd`, `None` outside git.
    pub worktree_root: Option<PathBuf>,
    /// Root of the main repository (differs from `worktree_root` in a linked worktree).
    pub repo_root: Option<PathBuf>,
    /// Normalized origin `owner/repo`, else the repository root or directory name.
    pub project: String,
    /// Where the job came from.
    pub source: Source,
}

/// Reduces a remote URL to `owner/repo`, dropping scheme, credentials, host, `.git` and trailing
/// slashes. Case is preserved. Returns `None` when the text is not a recognisable remote or when
/// the result would still contain credentials (`@` or `:` in a path segment).
pub fn normalize_origin(url: &str) -> Option<String> {
    let url = url.trim();
    let path = if let Some((_, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next().unwrap_or("");
        if host.is_empty() {
            return None;
        }
        path
    } else {
        let (authority, path) = url.split_once(':')?;
        let host = authority.rsplit('@').next().unwrap_or("");
        let drive_letter = !authority.contains('@') && host.chars().count() < 2;
        if host.is_empty() || drive_letter || host.contains(['/', '\\']) {
            return None;
        }
        path
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_matches('/');
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() < 2
        || segments
            .iter()
            .any(|s| s.trim().is_empty() || s.contains(['@', ':']))
    {
        return None;
    }
    Some(segments.join("/"))
}

fn dir_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Project name: the normalized origin, else the repository root directory name, else the name of `dir`.
pub fn project_name(origin: Option<&str>, repo_root: Option<&Path>, dir: &Path) -> String {
    origin
        .and_then(normalize_origin)
        .unwrap_or_else(|| dir_name(repo_root.unwrap_or(dir)))
}

/// Trims a label and checks it is present, free of control characters and at
/// most 100 characters long.
pub fn validate_label(label: &str) -> Result<String, IdentityError> {
    let label = label.trim();
    if label.is_empty() {
        return Err(IdentityError::MissingLabel);
    }
    if label.chars().any(char::is_control) {
        return Err(IdentityError::LabelInvalid);
    }
    if label.chars().count() > MAX_LABEL_CHARS {
        return Err(IdentityError::LabelTooLong);
    }
    Ok(label.to_string())
}

/// Trims a note, drops an empty one and checks it is at most 200 characters long.
pub fn validate_note(note: Option<&str>) -> Result<Option<String>, IdentityError> {
    let Some(note) = note.map(str::trim).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    if note.chars().count() > MAX_NOTE_CHARS {
        return Err(IdentityError::NoteTooLong);
    }
    Ok(Some(note.to_string()))
}

pub(crate) fn sanitize_label(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_LABEL_CHARS)
        .collect()
}

/// Facts about the submitter collected from the OS and git by the CLI (spec 7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderFacts {
    /// OS user name.
    pub os_user: String,
    /// Host name.
    pub host: String,
    /// Submitter process id, `0` when unknown.
    pub sender_pid: u32,
    /// Submitter process start in unix milliseconds.
    pub sender_start: Option<u64>,
    /// Working directory.
    pub cwd: PathBuf,
    /// Current branch, `None` when detached or outside git.
    pub branch: Option<String>,
    /// Root of the worktree containing `cwd`, `None` outside git.
    pub worktree_root: Option<PathBuf>,
    /// Root of the main repository, `None` outside git.
    pub repo_root: Option<PathBuf>,
    /// Raw URL of the `origin` remote, `None` when absent.
    pub origin: Option<String>,
}

pub(crate) fn build_identity(
    label: String,
    note: Option<String>,
    facts: SenderFacts,
    source: Source,
    fallback_project: Option<&str>,
) -> Identity {
    let has_origin = facts.origin.as_deref().and_then(normalize_origin).is_some();
    let project = match fallback_project {
        _ if has_origin => project_name(
            facts.origin.as_deref(),
            facts.repo_root.as_deref(),
            &facts.cwd,
        ),
        Some(name) => name.to_string(),
        None => project_name(None, facts.repo_root.as_deref(), &facts.cwd),
    };
    Identity {
        label,
        note,
        os_user: facts.os_user,
        host: facts.host,
        sender_pid: facts.sender_pid,
        sender_start: facts.sender_start,
        cwd: facts.cwd,
        branch: facts.branch,
        worktree_root: facts.worktree_root,
        repo_root: facts.repo_root,
        project,
        source,
    }
}

/// Builds the identity of a terminal or agent session from collected facts. The label is required.
pub fn identity_from_session(
    label: &str,
    note: Option<&str>,
    facts: SenderFacts,
) -> Result<Identity, IdentityError> {
    let label = validate_label(label)?;
    let note = validate_note(note)?;
    Ok(build_identity(label, note, facts, Source::Session, None))
}

/// Whether `actor` may cancel a job submitted by `owner` (spec 7.1): the same label and the same
/// OS user, or `force`.
pub fn may_cancel(actor: &Identity, owner: &Identity, force: bool) -> bool {
    force || (actor.label == owner.label && actor.os_user == owner.os_user)
}
