//! The state directory of a role (spec 10.3): created closed to its owner; an existing one is
//! closed when it is open, and used as it is when it cannot be closed (a bind mount, a directory
//! of another user) unless that leaves it both foreign and open. Warnings are collected once per
//! directory and process and go to the role's diagnostics (or standard error for commands).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static WARNED: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Who controls an existing state directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirAccess {
    /// Owner principal: uid on Unix, SID on Windows; `None` when unknown.
    pub owner: Option<String>,
    /// The owner is the user of this process (on Windows also Administrators or SYSTEM).
    pub own: bool,
    /// Someone else may get in: group or other bits on Unix; an allow entry for another
    /// principal, or no access list at all, on Windows.
    pub open: bool,
    /// The mode (`755`) or the principals let in, for messages.
    pub detail: String,
}

/// What to do with an existing state directory after trying to close it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Use it.
    Use,
    /// Use it and say why it is not closed.
    Warn(String),
    /// Do not start: another user owns it and others may get in.
    Refuse(String),
}

/// Judges an existing state directory `dir` by `access` as it is after an attempt to close it;
/// `closing` is the error of that attempt, if any.
pub fn verdict(dir: &Path, access: &DirAccess, closing: Option<&str>) -> Verdict {
    let owner = access.owner.as_deref().unwrap_or("an unknown owner");
    let why = closing
        .map(|e| format!(" and could not be closed ({e})"))
        .unwrap_or_default();
    match (access.own, access.open) {
        (true, false) => Verdict::Use,
        (false, true) => Verdict::Refuse(format!(
            "state directory {} belongs to {owner}, lets others in ({}){why}; \
             run `localrunner init` as its owner, give it to this user (chown) \
             or choose another --state-dir",
            dir.display(),
            access.detail
        )),
        (true, true) => Verdict::Warn(format!(
            "state directory {} lets others in ({}){why}; it is used as it is",
            dir.display(),
            access.detail
        )),
        (false, false) => Verdict::Warn(format!(
            "state directory {} belongs to {owner}, not to this user ({}); it is used as it is",
            dir.display(),
            access.detail
        )),
    }
}

/// Makes `dir` reachable for its owner only: mode 0700 on Unix; on Windows a protected access
/// list for this user, Administrators and SYSTEM.
pub fn close_to_owner(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", dir.display()))
    }
    #[cfg(windows)]
    {
        let me = lr_net::current_os_principal().map_err(|e| e.to_string())?;
        lr_net::close_dir_to_owner(dir, &me).map_err(|e| e.to_string())
    }
}

/// Who controls `dir` now.
#[cfg(unix)]
pub fn access(dir: &Path) -> Result<DirAccess, String> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let me = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    let owner = meta.uid().to_string();
    let mode = meta.mode() & 0o777;
    Ok(DirAccess {
        own: owner == me,
        owner: Some(format!("uid {owner}")),
        open: mode & 0o077 != 0,
        detail: format!("mode {mode:o}"),
    })
}

/// Who controls `dir` now.
#[cfg(windows)]
pub fn access(dir: &Path) -> Result<DirAccess, String> {
    const ADMINISTRATORS: &str = "S-1-5-32-544";
    const SYSTEM: &str = "S-1-5-18";
    const CREATOR_OWNER: &str = "S-1-3-0";
    let me = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    let security = lr_net::file_security(dir).map_err(|e| e.to_string())?;
    let trusted = |sid: &str| {
        [me.as_str(), ADMINISTRATORS, SYSTEM]
            .iter()
            .any(|t| t.eq_ignore_ascii_case(sid))
    };
    let others: Option<Vec<String>> = security.allowed.map(|all| {
        let mut v: Vec<String> = all
            .into_iter()
            .filter(|s| !trusted(s) && !s.eq_ignore_ascii_case(CREATOR_OWNER))
            .collect();
        v.sort();
        v.dedup();
        v
    });
    let (open, detail) = match &others {
        None => (true, "no access list: everyone".to_string()),
        Some(v) if v.is_empty() => (false, "access for this user only".to_string()),
        Some(v) => (true, format!("access for {}", v.join(", "))),
    };
    Ok(DirAccess {
        own: security.owner.as_deref().is_some_and(trusted),
        owner: security.owner,
        open,
        detail,
    })
}

fn warn_once(dir: &Path, text: String) {
    let first = WARNED
        .lock()
        .map(|mut w| w.insert(dir.to_path_buf()))
        .unwrap_or(false);
    if first {
        if let Ok(mut pending) = PENDING.lock() {
            pending.push(text);
        }
    }
}

/// The state-directory warnings not reported yet; each is returned once per process.
pub fn take_warnings() -> Vec<String> {
    PENDING
        .lock()
        .map(|mut p| std::mem::take(&mut *p))
        .unwrap_or_default()
}

/// Writes the pending warnings to standard error (commands without a role logger).
pub fn print_warnings() {
    for w in take_warnings() {
        eprintln!("warning: {w}");
    }
}

/// The state directory `dir`: created when missing and then closed to its owner; an existing
/// one is closed when open, and judged by [`verdict`] when that fails. The broker service's
/// directory keeps the security `broker install-service` gave it.
pub fn ensure_state_dir(dir: &Path) -> Result<(), String> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let created = match std::fs::create_dir(dir) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => false,
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    if dir == crate::paths::service_root() {
        return Ok(());
    }
    if created {
        return close_to_owner(dir);
    }
    let closing = if access(dir)?.open {
        close_to_owner(dir).err()
    } else {
        None
    };
    match verdict(dir, &access(dir)?, closing.as_deref()) {
        Verdict::Use => Ok(()),
        Verdict::Warn(w) => {
            warn_once(dir, w);
            Ok(())
        }
        Verdict::Refuse(r) => Err(r),
    }
}
