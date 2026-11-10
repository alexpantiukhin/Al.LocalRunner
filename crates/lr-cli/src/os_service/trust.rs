//! The broker service's state directory must be controlled by the service and the
//! administrators only (spec 10.3): one that someone else created, linked elsewhere or opened
//! up is refused before anything is copied into it, then it is closed.

use std::path::Path;

/// What decides whether a file or directory can be trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// A symbolic link, junction or other reparse point.
    pub link: bool,
    /// Owner principal (SID on Windows, uid on Unix).
    pub owner: Option<String>,
    /// Who else may write: SIDs of explicit allow entries on Windows, `group` or `others` for
    /// write bits on Unix; `None` when anyone may (no DACL) or the list cannot be read.
    pub writers: Option<Vec<String>>,
}

/// `Ok` when `facts` show `path` is controlled by `trusted` principals only.
pub fn judge(path: &Path, facts: &Facts, trusted: &[String]) -> Result<(), String> {
    let known = |p: &str| trusted.iter().any(|t| t.eq_ignore_ascii_case(p));
    let fix = "remove it (or fix it as administrator) and install again";
    if facts.link {
        return Err(format!("{} is a link or junction; {fix}", path.display()));
    }
    match &facts.owner {
        Some(o) if known(o) => {}
        Some(o) => {
            return Err(format!(
                "{} is owned by {o}, not by an administrator or the service; {fix}",
                path.display()
            ))
        }
        None => return Err(format!("{} has no owner; {fix}", path.display())),
    }
    match &facts.writers {
        None => Err(format!(
            "{} lets anyone in or its access list cannot be read; {fix}",
            path.display()
        )),
        Some(w) => match w.iter().find(|w| !known(w)) {
            Some(other) => Err(format!(
                "{} grants access to {other}; {fix}",
                path.display()
            )),
            None => Ok(()),
        },
    }
}

/// The facts of `path` itself (a link is not followed).
#[cfg(windows)]
pub fn facts(path: &Path) -> Result<Facts, String> {
    use std::os::windows::fs::MetadataExt;
    const REPARSE_POINT: u32 = 0x400;
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let link = meta.file_type().is_symlink() || meta.file_attributes() & REPARSE_POINT != 0;
    let security = lr_net::file_security(path).map_err(|e| e.to_string())?;
    Ok(Facts {
        link,
        owner: security.owner,
        writers: security.explicit_allowed,
    })
}

/// The facts of `path` itself (a link is not followed).
#[cfg(unix)]
pub fn facts(path: &Path) -> Result<Facts, String> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut writers = Vec::new();
    if meta.mode() & 0o020 != 0 {
        writers.push("group".to_string());
    }
    if meta.mode() & 0o002 != 0 {
        writers.push("others".to_string());
    }
    Ok(Facts {
        link: meta.file_type().is_symlink(),
        owner: Some(meta.uid().to_string()),
        writers: Some(writers),
    })
}

/// Checks everything below `root` (not `root` itself); links are refused, never followed.
pub fn check_children(root: &Path, trusted: &[String]) -> Result<(), String> {
    let entries = std::fs::read_dir(root).map_err(|e| format!("{}: {e}", root.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        let f = facts(&path)?;
        judge(&path, &f, trusted)?;
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            check_children(&path, trusted)?;
        }
    }
    Ok(())
}

/// The principals that may control the service's files: Administrators and SYSTEM (Windows) or
/// root (Unix), the user running this (the administrator installing), and the service account
/// once it exists.
pub fn trusted_principals(account: &str) -> Vec<String> {
    let mut out: Vec<String> = if cfg!(windows) {
        vec!["S-1-5-32-544".into(), "S-1-5-18".into()]
    } else {
        vec!["0".into()]
    };
    out.extend(lr_net::current_os_principal().ok());
    out.extend(lr_net::account_principal(account).ok());
    out
}

/// `Ok` when `path` does not exist or can be trusted.
pub fn require_trusted(path: &Path, account: &str) -> Result<(), String> {
    if std::fs::symlink_metadata(path).is_err() {
        return Ok(());
    }
    judge(path, &facts(path)?, &trusted_principals(account))
}

/// Checks and closes the service state directory `path` (creating it when missing). Windows:
/// the directory itself is checked and closed first, so nobody can add to it, then everything
/// below is checked and finally reset to the directory's DACL and owner. Unix: the whole tree
/// is checked, the directory gets mode 0700.
pub fn secure_service_dir(path: &Path, account: &str) -> Result<(), String> {
    let trusted = trusted_principals(account);
    let exists = std::fs::symlink_metadata(path).is_ok();
    if exists {
        judge(path, &facts(path)?, &trusted)?;
    } else {
        std::fs::create_dir_all(path).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    #[cfg(windows)]
    {
        let sid = lr_net::account_principal(account)
            .map_err(|e| format!("service account {account}: {e}"))?;
        lr_net::lock_service_dir(path, &sid, false).map_err(|e| e.to_string())?;
        check_children(path, &trusted)?;
        lr_net::lock_service_dir(path, &sid, true).map_err(|e| e.to_string())
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if exists {
            check_children(path, &trusted)?;
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}
