//! `upgrade.lock` in the state directory: written by `upgrade` for as long as it replaces the
//! binary, read by `up` (which then refuses), by a starting runner (which then starts in
//! drain) and by `status`. A lock of a dead process, or of a pid that another process reuses,
//! is ignored.

use crate::paths::Layout;
use lr_abstractions::{Liveness, ProcessInspector};
use lr_core::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FILE: &str = "upgrade.lock";

/// What an `upgrade` records about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpgradeLock {
    /// Pid of the `upgrade` process.
    pub pid: u32,
    /// Start fingerprint of that process (`lr_exec::process_start`).
    pub start: u64,
    /// Version `upgrade` installs.
    pub target: String,
    /// When `upgrade` started the replacement.
    pub since: Timestamp,
}

impl UpgradeLock {
    /// The lock of this process for an upgrade to `target`.
    pub fn of_this_process(target: &str, since: Timestamp) -> UpgradeLock {
        let pid = std::process::id();
        UpgradeLock {
            pid,
            start: lr_exec::process_start(pid).unwrap_or(0),
            target: target.to_string(),
            since,
        }
    }
}

/// Where the lock of `layout` lives.
pub fn lock_path(layout: &Layout) -> PathBuf {
    layout.root.join(FILE)
}

/// Removes the lock file when dropped.
#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Writes `lock` atomically (a temporary file renamed over the lock) and returns the guard that
/// removes it.
pub fn write(layout: &Layout, lock: &UpgradeLock) -> Result<LockGuard, String> {
    let path = lock_path(layout);
    let text = serde_json::to_string(lock).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("lock.tmp");
    std::fs::create_dir_all(&layout.root).map_err(|e| format!("{}: {e}", layout.root.display()))?;
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(LockGuard { path })
}

fn read(path: &Path) -> Option<UpgradeLock> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The lock of `layout` when its process is alive; `None` for a missing, unreadable or dead
/// one. A process whose start fingerprint cannot be read counts as alive.
pub fn live(layout: &Layout, inspector: &dyn ProcessInspector) -> Option<UpgradeLock> {
    let lock = read(&lock_path(layout))?;
    match inspector.liveness(lock.pid, lock.start) {
        Liveness::Dead => None,
        Liveness::Alive | Liveness::Unknown => Some(lock),
    }
}

/// [`live`] with the inspector of this machine.
pub fn live_here(layout: &Layout) -> Option<UpgradeLock> {
    live(layout, &lr_exec::OsProcessInspector)
}

/// Environment variable `upgrade` sets for the roles it starts: its own pid, so they do not
/// treat its lock as a reason to start in drain.
pub const OWNER_ENV: &str = "LOCALRUNNER_UPGRADE_LOCK_PID";

/// The live lock that concerns a process with pid `me` started by `owner` (the value of
/// [`OWNER_ENV`]): the lock of the upgrade that is not this process and did not start it.
pub fn blocking(
    layout: &Layout,
    inspector: &dyn ProcessInspector,
    me: u32,
    owner: Option<u32>,
) -> Option<UpgradeLock> {
    live(layout, inspector).filter(|l| l.pid != me && Some(l.pid) != owner)
}

/// [`blocking`] for this process, with the inspector and the environment of this machine.
pub fn blocking_here(layout: &Layout) -> Option<UpgradeLock> {
    let owner = std::env::var(OWNER_ENV).ok().and_then(|v| v.parse().ok());
    blocking(
        layout,
        &lr_exec::OsProcessInspector,
        std::process::id(),
        owner,
    )
}

/// One line for `up`, `status` and `runners`.
pub fn describe(lock: &UpgradeLock) -> String {
    format!(
        "upgrade to {} runs (pid {}, since {})",
        lock.target,
        lock.pid,
        crate::commands::timefmt::format_time(lock.since)
    )
}
