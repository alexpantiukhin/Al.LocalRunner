//! The guard of `uninstall --purge` (spec 10.5): the state directory is deleted only when it
//! is one, never a file-system root, the home directory, a directory holding the binary or the
//! current directory, and never through a symbolic link.

use crate::host::MACHINE_ID_FILE;
use std::path::{Component, Path, PathBuf};

/// The directories `--purge` must never delete or hold.
#[derive(Debug, Clone, Default)]
pub struct Protected {
    /// The home directory of the user.
    pub home: Option<PathBuf>,
    /// The directories with the binaries (the install directory, the running binary's).
    pub bins: Vec<PathBuf>,
    /// The current directory.
    pub cwd: Option<PathBuf>,
}

impl Protected {
    /// The directories of this process: `HOME`/`USERPROFILE`, `bin_dir` and the directory of
    /// the running binary, the current directory.
    pub fn of_process(bin_dir: &Path) -> Protected {
        let mut bins = vec![bin_dir.to_path_buf()];
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_path_buf))
        {
            bins.push(dir);
        }
        Protected {
            home: crate::os_service::home_dir().ok(),
            bins,
            cwd: std::env::current_dir().ok(),
        }
    }
}

fn clean(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for c in absolute.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn key(c: Component) -> String {
    let text = c.as_os_str().to_string_lossy();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text.into_owned()
    }
}

fn holds(outer: &Path, inner: &Path) -> bool {
    let (outer, inner) = (clean(outer), clean(inner));
    let mut o = outer.components();
    let mut i = inner.components();
    loop {
        match (o.next(), i.next()) {
            (None, _) => return true,
            (Some(_), None) => return false,
            (Some(a), Some(b)) if key(a) == key(b) => {}
            _ => return false,
        }
    }
}

/// The directory `uninstall --purge` would delete for the state directory `root`, or why it
/// refuses: a file-system root, the home directory or one holding it, a directory holding a
/// binary directory or the current directory, a symbolic link, or a directory without the
/// `machine-id` that `init` and every role create.
pub fn purge_target(root: &Path, protected: &Protected) -> Result<PathBuf, String> {
    let path = clean(root);
    let refuse = |why: String| {
        Err(format!(
            "--purge refuses to delete {}: {why}",
            path.display()
        ))
    };
    if path.parent().is_none() {
        return refuse("it is the root of a file system".into());
    }
    if let Some(home) = &protected.home {
        if holds(&path, home) {
            return refuse(format!(
                "it is or holds the home directory {}",
                home.display()
            ));
        }
    }
    if let Some(bin) = protected.bins.iter().find(|b| holds(&path, b)) {
        return refuse(format!(
            "it is or holds the binary directory {}",
            bin.display()
        ));
    }
    if let Some(cwd) = &protected.cwd {
        if holds(&path, cwd) {
            return refuse(format!(
                "it is or holds the current directory {}",
                cwd.display()
            ));
        }
    }
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_symlink() => refuse("it is a symbolic link".into()),
        Ok(m) if !m.is_dir() => refuse("it is not a directory".into()),
        Ok(_) => match std::fs::symlink_metadata(path.join(MACHINE_ID_FILE)) {
            Ok(m) if m.is_file() => Ok(path),
            _ => refuse(format!(
                "it is not a state directory (no {MACHINE_ID_FILE})"
            )),
        },
        Err(e) => refuse(e.to_string()),
    }
}
