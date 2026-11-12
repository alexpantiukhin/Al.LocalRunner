//! The guard of `uninstall --purge` (spec 10.5): the state directory is deleted only when it
//! is one, never a file-system root, the home directory, a directory holding the binary or the
//! current directory, and never through a symbolic link. Paths are compared as written and by
//! file-system identity, so another spelling of the same directory (case, `\\?\`, 8.3 names,
//! links among the parents, `..`) is refused as well.

use crate::host::MACHINE_ID_FILE;
use std::path::{Component, Path, PathBuf};

/// The directories `--purge` must never delete or hold.
#[derive(Debug, Clone, Default)]
pub struct Protected {
    /// The home directories of the user (`HOME` and `USERPROFILE`).
    pub homes: Vec<PathBuf>,
    /// The directories with the binaries (the install directory, the running binary's).
    pub bins: Vec<PathBuf>,
    /// The current directory.
    pub cwd: Option<PathBuf>,
}

impl Protected {
    /// The directories of this process: `HOME` and `USERPROFILE`, `bin_dir` and the directory
    /// of the running binary, the current directory.
    pub fn of_process(bin_dir: &Path) -> Protected {
        let mut bins = vec![bin_dir.to_path_buf()];
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_path_buf))
        {
            bins.push(dir);
        }
        let homes = ["HOME", "USERPROFILE"]
            .iter()
            .filter_map(std::env::var_os)
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .collect();
        Protected {
            homes,
            bins,
            cwd: std::env::current_dir().ok(),
        }
    }

    fn named(&self) -> Vec<(&'static str, &PathBuf)> {
        let homes = self.homes.iter().map(|p| ("home directory", p));
        let bins = self.bins.iter().map(|p| ("binary directory", p));
        let cwd = self.cwd.iter().map(|p| ("current directory", p));
        homes.chain(bins).chain(cwd).collect()
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

fn without_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

fn canonical(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok().map(without_verbatim)
}

fn key(c: Component) -> String {
    let text = c.as_os_str().to_string_lossy();
    if cfg!(any(windows, target_os = "macos")) {
        text.to_lowercase()
    } else {
        text.into_owned()
    }
}

fn holds(outer: &Path, inner: &Path) -> bool {
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

#[cfg(unix)]
fn identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(windows)]
fn identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(handle, &mut info) } != 0;
    unsafe { CloseHandle(handle) };
    ok.then(|| {
        (
            u64::from(info.dwVolumeSerialNumber),
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        )
    })
}

fn same_or_above(target: &Path, target_id: Option<(u64, u64)>, protected: &Path) -> bool {
    if holds(&clean(target), &clean(protected)) {
        return true;
    }
    let Some(inner) = canonical(protected) else {
        return false;
    };
    if canonical(target).is_some_and(|outer| holds(&outer, &inner)) {
        return true;
    }
    target_id.is_some_and(|id| inner.ancestors().any(|a| identity(a) == Some(id)))
}

/// The directory `uninstall --purge` would delete for the state directory `root`, or why it
/// refuses: a file-system root, a home directory or one holding it, a directory holding a
/// binary directory or the current directory (by spelling, canonical path or file-system
/// identity), a symbolic link or junction, or a directory without the `machine-id` that
/// `init` and every role create.
pub fn purge_target(root: &Path, protected: &Protected) -> Result<PathBuf, String> {
    let path = clean(root);
    let refuse = |why: String| {
        Err(format!(
            "--purge refuses to delete {}: {why}",
            path.display()
        ))
    };
    if path.parent().is_none() || canonical(&path).is_some_and(|c| c.parent().is_none()) {
        return refuse("it is the root of a file system".into());
    }
    for (what, p) in protected.named() {
        if holds(&path, &clean(p)) {
            return refuse(format!("it is or holds the {what} {}", p.display()));
        }
    }
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_symlink() => return refuse("it is a symbolic link".into()),
        Ok(m) if !m.is_dir() => return refuse("it is not a directory".into()),
        Ok(_) => {}
        Err(e) => return refuse(e.to_string()),
    }
    let id = identity(&path);
    for (what, p) in protected.named() {
        if same_or_above(&path, id, p) {
            return refuse(format!("it is or holds the {what} {}", p.display()));
        }
    }
    match std::fs::symlink_metadata(path.join(MACHINE_ID_FILE)) {
        Ok(m) if m.is_file() => Ok(path),
        _ => refuse(format!(
            "it is not a state directory (no {MACHINE_ID_FILE})"
        )),
    }
}
