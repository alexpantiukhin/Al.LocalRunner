//! The real [`Repository`]: `git` run as a child process without a shell, every value checked
//! before it reaches the command line (spec 13.3, 13.4).

use async_trait::async_trait;
use lr_abstractions::{Repository, RepositoryError};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Reads commits, push state and committed files with the `git` of `PATH`; it never changes
/// the repository.
#[derive(Debug, Clone, Copy, Default)]
pub struct GitRepository;

fn bad(text: impl Into<String>) -> RepositoryError {
    RepositoryError(text.into())
}

fn check_root(root: &Path) -> Result<PathBuf, RepositoryError> {
    if !root.is_absolute() {
        return Err(bad(format!("{} is not an absolute path", root.display())));
    }
    Ok(root.to_path_buf())
}

fn check_commit(commit: &str) -> Result<(), RepositoryError> {
    let ok = (7..=64).contains(&commit.len()) && commit.bytes().all(|b| b.is_ascii_hexdigit());
    if ok {
        Ok(())
    } else {
        Err(bad(format!("`{commit}` is not a commit hash")))
    }
}

fn check_path(path: &str) -> Result<(), RepositoryError> {
    let chars_ok = path
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'));
    let parts_ok = path
        .split('/')
        .all(|p| !p.is_empty() && p != "." && p != ".." && !p.starts_with('-'));
    if chars_ok && parts_ok && path.len() <= 1024 {
        Ok(())
    } else {
        Err(bad(format!("`{path}` is not a plain relative path")))
    }
}

/// `url` without credentials: an `http(s)` URL loses its whole userinfo (`user:TOKEN@`,
/// `TOKEN@`), any other `scheme://` URL keeps only the user name; scp-like addresses
/// (`git@host:path`) carry no secret and stay as they are. The origin travels to the broker
/// and to runners of other machines, so a token must not.
pub fn without_credentials(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let Some((userinfo, host)) = authority.rsplit_once('@') else {
        return url.to_string();
    };
    let web = scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https");
    match userinfo.split_once(':').map(|(u, _)| u).or(Some(userinfo)) {
        Some(user) if !web && !user.is_empty() => format!("{scheme}://{user}@{host}{tail}"),
        _ => format!("{scheme}://{host}{tail}"),
    }
}

/// A `git -C <cwd>` command that ignores the caller's repository overrides: every `GIT_*`
/// variable of `env` (any case) is removed and `GIT_TERMINAL_PROMPT=0` set, so neither
/// `GIT_DIR`, `GIT_COMMON_DIR`, `GIT_CONFIG_*` nor the like can point git elsewhere.
pub fn git_command_in<I>(cwd: &Path, env: I) -> Command
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    let mut cmd = Command::new("git");
    for var in env {
        if var
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            cmd.env_remove(var);
        }
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0").arg("-C").arg(cwd);
    cmd
}

/// [`git_command_in`] over the environment of this process.
pub fn git_command(cwd: &Path) -> Command {
    git_command_in(cwd, std::env::vars_os().map(|(k, _)| k))
}

struct Output {
    ok: bool,
    stdout: Vec<u8>,
    stderr: String,
}

async fn git(root: PathBuf, args: Vec<String>) -> Result<Output, RepositoryError> {
    tokio::task::spawn_blocking(move || {
        let out = git_command(&root)
            .args(&args)
            .output()
            .map_err(|e| bad(format!("cannot run git: {e}")))?;
        Ok(Output {
            ok: out.status.success(),
            stdout: out.stdout,
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        })
    })
    .await
    .map_err(|e| bad(e.to_string()))?
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

#[async_trait]
impl Repository for GitRepository {
    async fn head(&self, root: &Path) -> Result<Option<String>, RepositoryError> {
        let root = check_root(root)?;
        let out = git(
            root,
            vec![
                "rev-parse".into(),
                "--verify".into(),
                "-q".into(),
                "HEAD".into(),
            ],
        )
        .await?;
        if !out.ok {
            return Ok(None);
        }
        let sha = text(&out.stdout);
        check_commit(&sha)?;
        Ok(Some(sha))
    }

    async fn is_pushed(&self, root: &Path, commit: &str) -> Result<bool, RepositoryError> {
        let root = check_root(root)?;
        check_commit(commit)?;
        let out = git(
            root,
            vec![
                "for-each-ref".into(),
                format!("--contains={commit}"),
                "--format=%(refname)".into(),
                "refs/remotes".into(),
            ],
        )
        .await?;
        if !out.ok {
            return Err(bad(format!("git for-each-ref: {}", out.stderr)));
        }
        Ok(!text(&out.stdout).is_empty())
    }

    async fn show(
        &self,
        root: &Path,
        commit: &str,
        path: &str,
    ) -> Result<Option<Vec<u8>>, RepositoryError> {
        let root = check_root(root)?;
        check_commit(commit)?;
        check_path(path)?;
        let object = format!("{commit}:{path}");
        let exists = git(
            root.clone(),
            vec!["cat-file".into(), "-e".into(), object.clone()],
        )
        .await?;
        if !exists.ok {
            return Ok(None);
        }
        let out = git(root, vec!["cat-file".into(), "blob".into(), object]).await?;
        if !out.ok {
            return Err(bad(format!("git cat-file: {}", out.stderr)));
        }
        Ok(Some(out.stdout))
    }

    async fn origin_url(&self, root: &Path) -> Result<Option<String>, RepositoryError> {
        let root = check_root(root)?;
        let out = git(
            root,
            vec!["remote".into(), "get-url".into(), "origin".into()],
        )
        .await?;
        Ok(out
            .ok
            .then(|| without_credentials(&text(&out.stdout)))
            .filter(|u| !u.is_empty()))
    }
}
