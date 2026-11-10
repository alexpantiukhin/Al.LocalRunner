use lr_core::{identity_from_github, identity_from_session, Identity, IdentityError, SenderFacts};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

pub(crate) struct GitFacts {
    pub branch: Option<String>,
    pub worktree_root: PathBuf,
    pub repo_root: PathBuf,
    pub origin: Option<String>,
}

fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = crate::git_repo::git_command(cwd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout)
        .trim_end_matches(['\n', '\r'])
        .to_string();
    (!text.is_empty()).then_some(text)
}

fn native_path(text: &str) -> PathBuf {
    Path::new(text).components().collect()
}

fn main_root(common_dir: &Path, fallback: &Path) -> PathBuf {
    let ends_with_git = common_dir
        .components()
        .next_back()
        .is_some_and(|c| matches!(c, Component::Normal(name) if name == ".git"));
    match common_dir.parent() {
        Some(parent) if ends_with_git && !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => fallback.to_path_buf(),
    }
}

pub(crate) fn git_facts(cwd: &Path) -> Option<GitFacts> {
    let worktree_root = native_path(&run_git(cwd, &["rev-parse", "--show-toplevel"])?);
    let branch = run_git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD");
    let origin = run_git(cwd, &["remote", "get-url", "origin"])
        .map(|o| crate::git_repo::without_credentials(&o));
    let repo_root = run_git(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map(|dir| main_root(&native_path(&dir), &worktree_root))
    .unwrap_or_else(|| worktree_root.clone());
    Some(GitFacts {
        branch,
        worktree_root,
        repo_root,
        origin,
    })
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

pub fn os_user() -> String {
    env_nonempty("USERNAME")
        .or_else(|| env_nonempty("USER"))
        .unwrap_or_else(|| "unknown".to_string())
}

pub(crate) fn host_name() -> String {
    env_nonempty("COMPUTERNAME")
        .or_else(|| env_nonempty("HOSTNAME"))
        .or_else(|| {
            let out = Command::new("hostname").output().ok()?;
            let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (out.status.success() && !name.is_empty()).then_some(name)
        })
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(windows)]
fn parent_pid() -> Option<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let own = std::process::id();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = None;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            if entry.th32ProcessID == own {
                found = Some(entry.th32ParentProcessID);
                break;
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        found
    }
}

#[cfg(unix)]
fn parent_pid() -> Option<u32> {
    unix_parent(std::os::unix::process::parent_id())
}

#[cfg(unix)]
fn unix_parent(ppid: u32) -> Option<u32> {
    (ppid > 1).then_some(ppid)
}

#[cfg(not(any(windows, unix)))]
fn parent_pid() -> Option<u32> {
    None
}

/// The process that started this one, with its start fingerprint from
/// [`lr_exec::process_start`], the value the hub's dead-submitter rule compares
/// (spec 7.3). `None` when the
/// parent is unknown, already gone (on Unix the process was handed to init,
/// pid 1), or its pid was reused by a process that started after this one.
pub(crate) fn parent_process() -> Option<(u32, Option<u64>)> {
    let parent = parent_pid().filter(|&p| p != 0 && p != std::process::id())?;
    let start = lr_exec::process_start(parent);
    if cfg!(windows) {
        let start = start?;
        let own = lr_exec::process_start(std::process::id())?;
        if start > own {
            return None;
        }
    }
    Some((parent, start))
}

/// Collects OS and git facts about the submitter running in `cwd`.
pub fn collect_facts(cwd: &Path) -> SenderFacts {
    let git = git_facts(cwd);
    let (sender_pid, sender_start) = parent_process().unwrap_or((0, None));
    SenderFacts {
        os_user: os_user(),
        host: host_name(),
        sender_pid,
        sender_start,
        cwd: cwd.to_path_buf(),
        branch: git.as_ref().and_then(|g| g.branch.clone()),
        worktree_root: git.as_ref().map(|g| g.worktree_root.clone()),
        repo_root: git.as_ref().map(|g| g.repo_root.clone()),
        origin: git.and_then(|g| g.origin),
    }
}

/// Collects the identity of a terminal or agent session. The label is required.
pub fn collect_session(
    label: &str,
    note: Option<&str>,
    cwd: &Path,
) -> Result<Identity, IdentityError> {
    identity_from_session(label, note, collect_facts(cwd))
}

/// Collects the identity of a GitHub Actions job from an explicit environment map.
pub fn collect_github(
    env: &BTreeMap<String, String>,
    label: Option<&str>,
    note: Option<&str>,
    cwd: &Path,
) -> Result<Identity, IdentityError> {
    identity_from_github(env, label, note, collect_facts(cwd))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn unix_orphan_has_no_parent() {
        assert_eq!(unix_parent(0), None);
        assert_eq!(unix_parent(1), None);
        assert_eq!(unix_parent(4242), Some(4242));
    }

    #[test]
    fn facts_and_repository_scrub_every_git_variable() {
        let env = [
            "GIT_DIR",
            "GIT_COMMON_DIR",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
            "git_object_directory",
            "PATH",
        ]
        .map(std::ffi::OsString::from);
        let cmd = crate::git_repo::git_command_in(Path::new("."), env);
        let removed: Vec<String> = cmd
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        for var in [
            "GIT_DIR",
            "GIT_COMMON_DIR",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
        ] {
            assert!(
                removed.iter().any(|r| r.eq_ignore_ascii_case(var)),
                "{var} not scrubbed"
            );
        }
        assert!(removed
            .iter()
            .any(|r| r.eq_ignore_ascii_case("git_object_directory")));
        assert!(!removed.iter().any(|r| r == "PATH"));
        let prompt = cmd
            .get_envs()
            .find(|(k, _)| *k == "GIT_TERMINAL_PROMPT")
            .and_then(|(_, v)| v);
        assert_eq!(prompt, Some(std::ffi::OsStr::new("0")));
    }
}
