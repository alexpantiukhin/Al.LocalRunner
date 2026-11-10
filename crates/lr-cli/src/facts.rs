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

/// Start fingerprint of a process in milliseconds; `None` when the process
/// is gone or cannot be inspected. Compare it only with another value from
/// this function on the same machine. Windows: creation time in unix
/// milliseconds, as stored by the kernel. Linux: milliseconds since boot from
/// `/proc/<pid>/stat`, independent of the wall clock. Other Unix: now minus
/// the elapsed time `ps` reports, so two readings drift by about a second and
/// a step of the wall clock shifts every later reading; see
/// [`process_start_is_stable`].
#[cfg(windows)]
pub fn process_start_ms(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const UNIX_EPOCH_AS_FILETIME_MS: u64 = 11_644_473_600_000;
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    let ok = unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let ok = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
        CloseHandle(handle);
        ok
    };
    if ok == 0 {
        return None;
    }
    let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    (ticks / 10_000).checked_sub(UNIX_EPOCH_AS_FILETIME_MS)
}

/// Start fingerprint of a process in milliseconds since boot, read from
/// field 22 of `/proc/<pid>/stat`; `None` when the process is gone.
#[cfg(target_os = "linux")]
pub fn process_start_ms(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let ticks: u64 = parse_stat_starttime(&stat)?;
    let per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let per_second = u64::try_from(per_second).ok().filter(|&t| t > 0)?;
    Some(ticks.saturating_mul(1000) / per_second)
}

#[cfg(any(target_os = "linux", test))]
fn parse_stat_starttime(stat: &str) -> Option<u64> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// Start fingerprint of a process: now minus the elapsed time `ps` reports;
/// `None` when the process is gone or cannot be inspected.
#[cfg(all(unix, not(target_os = "linux")))]
pub fn process_start_ms(pid: u32) -> Option<u64> {
    let out = Command::new("ps")
        .args(["-o", "etime=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let elapsed = parse_etime(String::from_utf8_lossy(&out.stdout).trim())?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    now.checked_sub(elapsed * 1000)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn parse_etime(text: &str) -> Option<u64> {
    let (days, clock) = match text.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, text),
    };
    let mut seconds = 0;
    let parts: Vec<&str> = clock.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    for part in parts {
        seconds = seconds * 60 + part.parse::<u64>().ok()?;
    }
    Some(days * 86_400 + seconds)
}

/// True when [`process_start_ms`] of one process gives the same value on
/// every reading whatever happens to the wall clock (Windows, Linux). When
/// false, a different reading does not prove the pid was reused.
pub fn process_start_is_stable() -> bool {
    cfg!(any(windows, target_os = "linux"))
}

/// Start time of a process; not available on this platform.
#[cfg(not(any(windows, unix)))]
pub fn process_start_ms(_pid: u32) -> Option<u64> {
    None
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

/// The process that started this one, with its start time. `None` when the
/// parent is unknown, already gone (on Unix the process was handed to init,
/// pid 1), or its pid was reused by a process that started after this one.
pub(crate) fn parent_process() -> Option<(u32, Option<u64>)> {
    let parent = parent_pid().filter(|&p| p != 0 && p != std::process::id())?;
    let start = process_start_ms(parent);
    if cfg!(windows) {
        let start = start?;
        let own = process_start_ms(std::process::id())?;
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

    #[test]
    fn stat_starttime_is_field_22_after_the_command() {
        let stat = "4242 (odd ) name) S 1 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 987654 1000 10";
        assert_eq!(parse_stat_starttime(stat), Some(987_654));
        assert_eq!(parse_stat_starttime("garbage"), None);
    }

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
