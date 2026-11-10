use lr_core::{JobId, StepSpec};
use lr_proto::WireCode;
use std::fmt;
use std::path::{Path, PathBuf};

/// Name of a profile step that replaces the default `source` sequence.
pub const SOURCE_STEP: &str = "source";

const MAX_URL: usize = 2048;
const MAX_BRANCH: usize = 255;
const NAME_PART: usize = 48;
const URL_CHARS: &str = "._~:/@+=,-";
const SCHEMES: [&str; 5] = ["https", "http", "ssh", "git", "file"];
const SHELL_SPECIALS: &str = "\"$`%!^";

/// Why the source of a job cannot be prepared; the job then fails with this text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceError(pub String);

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "source: {}", self.0)
    }
}

impl std::error::Error for SourceError {}

fn err<T>(text: impl Into<String>) -> Result<T, SourceError> {
    Err(SourceError(text.into()))
}

/// Checks a repository address before it reaches a command: one of the schemes `https`,
/// `http`, `ssh`, `git`, `file`, or the scp form `user@host:path`; no option-like start, no
/// remote helper (`transport::address`), and only characters that need no shell escaping.
pub fn validate_repo_url(url: &str) -> Result<(), SourceError> {
    if url.is_empty() || url.len() > MAX_URL {
        return err("repository address is empty or too long");
    }
    if url.starts_with('-') || url.contains("::") || url.contains("@-") || url.contains("://-") {
        return err("repository address looks like an option or a remote helper");
    }
    if !url
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || URL_CHARS.contains(c))
    {
        return err("repository address has a character outside the allowed set");
    }
    if let Some((scheme, rest)) = url.split_once("://") {
        if !SCHEMES.contains(&scheme) || rest.is_empty() {
            return err(format!("repository scheme `{scheme}` is not allowed"));
        }
        if scheme != "file" && rest.split('/').next().is_none_or(str::is_empty) {
            return err("repository address has no host");
        }
        return Ok(());
    }
    match url.split_once(':') {
        Some((authority, path))
            if authority.rsplit('@').next().is_some_and(|h| h.len() >= 2)
                && !authority.contains('/')
                && !path.is_empty() =>
        {
            Ok(())
        }
        _ => err("repository address is neither a URL nor user@host:path"),
    }
}

/// Checks a commit: a full or abbreviated hexadecimal object name of 7 to 64 digits.
pub fn validate_commit(sha: &str) -> Result<(), SourceError> {
    if (7..=64).contains(&sha.len()) && sha.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        err("commit is not a hexadecimal object name")
    }
}

/// Checks a branch name by the rules of git references, restricted to `[A-Za-z0-9._/-]`.
pub fn validate_branch(branch: &str) -> Result<(), SourceError> {
    let ok = !branch.is_empty()
        && branch.len() <= MAX_BRANCH
        && branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c))
        && !branch.starts_with(['-', '/'])
        && !branch.ends_with(['/', '.'])
        && !branch.ends_with(".lock")
        && !branch.contains("..")
        && !branch.contains("//");
    if ok {
        Ok(())
    } else {
        err("branch is not a valid reference name")
    }
}

/// Quotes one argument for the step shell (`/bin/sh` or `cmd.exe`) in double quotes. Characters
/// that keep a meaning inside double quotes in either shell (`"`, `$`, `` ` ``, `%`, `!`, `^`),
/// control characters and a trailing backslash are refused instead of escaped; a backslash is
/// accepted only on Windows, where it separates path components.
pub fn quote_arg(arg: &str) -> Result<String, SourceError> {
    let backslash_ok = cfg!(windows) && !arg.ends_with('\\');
    let bad = arg
        .chars()
        .any(|c| c.is_control() || SHELL_SPECIALS.contains(c) || (c == '\\' && !backslash_ok));
    if arg.is_empty() || bad {
        return err("argument has a character the step shell would interpret");
    }
    Ok(format!("\"{arg}\""))
}

fn fnv(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for b in part.bytes().chain(std::iter::once(0)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

pub(crate) fn dir_name(label: &str, key: &[&str]) -> String {
    let clean: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(NAME_PART)
        .collect();
    format!("{clean}-{:016x}", fnv(key))
}

/// Where remote jobs are checked out: `<state>/workspace/<project>/mirror.git` (one bare mirror
/// per project and repository address) and `<state>/workspace/<project>/jobs/<job>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLayout {
    root: PathBuf,
}

impl WorkspaceLayout {
    /// The layout inside the runner's state directory.
    pub fn new(state_dir: PathBuf) -> WorkspaceLayout {
        WorkspaceLayout {
            root: state_dir.join("workspace"),
        }
    }

    /// The `{workspace}` of a project fetched from `url`.
    pub fn project_dir(&self, project: &str, url: &str) -> PathBuf {
        self.root.join(dir_name(project, &[project, url]))
    }

    /// The bare mirror of the project.
    pub fn mirror_dir(&self, project: &str, url: &str) -> PathBuf {
        self.project_dir(project, url).join("mirror.git")
    }

    /// The `{job_dir}`: the checkout of one job of one client, where the profile steps start.
    pub fn job_dir(&self, project: &str, url: &str, client: &str, job: &JobId) -> PathBuf {
        self.project_dir(project, url)
            .join("jobs")
            .join(dir_name(&job.0, &[client, &job.0]))
    }
}

/// What the order says about its code and steps.
pub struct SourceInput<'a> {
    /// The job.
    pub job: &'a JobId,
    /// Client key.
    pub client: &'a str,
    /// Project.
    pub project: &'a str,
    /// Code source.
    pub code: &'a WireCode,
    /// Steps of the order.
    pub steps: &'a [StepSpec],
    /// Job timeout.
    pub timeout_s: u64,
}

/// How the working directory of a job is prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePlan {
    /// Directories the runner creates before the source steps (the `mkdir` of the sequence).
    pub make_dirs: Vec<PathBuf>,
    /// Source steps, run in [`SourcePlan::source_cwd`]; empty for a work tree.
    pub source_steps: Vec<StepSpec>,
    /// Whether the source steps come from the profile (they are order steps with results).
    pub from_profile: bool,
    /// Working directory of the source steps.
    pub source_cwd: PathBuf,
    /// Working directory of the profile steps.
    pub job_cwd: PathBuf,
    /// Position of the first profile step in the order.
    pub first_ord: usize,
    /// The checkout removed when the job ends.
    pub remove_after: Option<PathBuf>,
}

fn git_step(name: &str, command: String, timeout_s: u64) -> StepSpec {
    StepSpec {
        name: name.to_string(),
        command,
        timeout_s,
        mem_mb: None,
        cpu: None,
        tags: None,
        secrets: Vec::new(),
        continue_on_failure: false,
    }
}

fn path_arg(p: &Path) -> Result<String, SourceError> {
    quote_arg(&p.display().to_string())
}

struct Values {
    repo_url: String,
    commit: String,
    branch: Option<String>,
    workspace: String,
    job_dir: String,
}

fn substitute(command: &str, v: &Values) -> Result<String, SourceError> {
    let mut out = command
        .replace("{repo_url}", &v.repo_url)
        .replace("{commit}", &v.commit)
        .replace("{workspace}", &v.workspace)
        .replace("{job_dir}", &v.job_dir);
    if out.contains("{branch}") {
        let Some(b) = &v.branch else {
            return err("the profile source step uses {branch}, the order has none");
        };
        out = out.replace("{branch}", b);
    }
    Ok(out)
}

/// Plans the `source` step (spec 3.7). A work tree job only changes directory into the
/// submitter's work tree. A commit job uses the project mirror of `layout`: when
/// `mirror_exists` is false the runner creates the workspace and clones the mirror, then it
/// fetches, prunes stale checkouts and adds a detached checkout of the commit for this job.
/// Leading order steps named [`SOURCE_STEP`] replace that default sequence; the runner fills
/// their `{repo_url}`, `{commit}`, `{branch}`, `{workspace}` and `{job_dir}`. Every value is
/// validated and quoted before it becomes part of a command.
pub fn plan_source(
    layout: &WorkspaceLayout,
    input: &SourceInput<'_>,
    mirror_exists: bool,
) -> Result<SourcePlan, SourceError> {
    let (sha, repo_url, branch) = match input.code {
        WireCode::WorkTree { cwd } => {
            return Ok(SourcePlan {
                make_dirs: Vec::new(),
                source_steps: Vec::new(),
                from_profile: false,
                source_cwd: PathBuf::from(cwd),
                job_cwd: PathBuf::from(cwd),
                first_ord: 0,
                remove_after: None,
            })
        }
        WireCode::Commit {
            sha,
            repo_url,
            branch,
        } => (sha, repo_url, branch),
    };
    let Some(url) = repo_url else {
        return err("the order has no repository address");
    };
    validate_repo_url(url)?;
    validate_commit(sha)?;
    if let Some(b) = branch {
        validate_branch(b)?;
    }
    let ws = layout.project_dir(input.project, url);
    let mirror = layout.mirror_dir(input.project, url);
    let job_dir = layout.job_dir(input.project, url, input.client, input.job);
    let overrides = input
        .steps
        .iter()
        .take_while(|s| s.name == SOURCE_STEP)
        .count();
    let t = input.timeout_s;
    let source_steps = if overrides > 0 {
        let values = Values {
            repo_url: quote_arg(url)?,
            commit: sha.clone(),
            branch: branch.as_deref().map(quote_arg).transpose()?,
            workspace: path_arg(&ws)?,
            job_dir: path_arg(&job_dir)?,
        };
        input.steps[..overrides]
            .iter()
            .map(|s| {
                Ok(StepSpec {
                    command: substitute(&s.command, &values)?,
                    ..s.clone()
                })
            })
            .collect::<Result<Vec<_>, SourceError>>()?
    } else {
        let m = path_arg(&mirror)?;
        let mut steps = Vec::new();
        if !mirror_exists {
            steps.push(git_step(
                "clone",
                format!("git clone --mirror -- {} {m}", quote_arg(url)?),
                t,
            ));
        }
        steps.push(git_step(
            "fetch",
            format!("git -C {m} fetch --prune -- origin"),
            t,
        ));
        steps.push(git_step("prune", format!("git -C {m} worktree prune"), t));
        steps.push(git_step(
            "checkout",
            format!(
                "git -C {m} worktree add --detach -- {} {sha}",
                path_arg(&job_dir)?
            ),
            t,
        ));
        steps
    };
    Ok(SourcePlan {
        make_dirs: vec![
            ws.clone(),
            job_dir.parent().map_or(ws.clone(), Path::to_path_buf),
        ],
        source_steps,
        from_profile: overrides > 0,
        source_cwd: ws,
        job_cwd: job_dir.clone(),
        first_ord: overrides,
        remove_after: Some(job_dir),
    })
}
