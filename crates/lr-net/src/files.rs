//! Secret files (broker certificate key, participant keys) readable by their owner only
//! (spec 10.3): mode `0600` on Unix, a protected DACL granting the current user alone on
//! Windows.

use crate::error::NetError;
use std::fs;
use std::io::Write;
use std::path::Path;

/// Writes `bytes` to `path` so that only the current OS user can read it. The file is written
/// next to its final name and renamed into place, so a reader never sees half a key.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), NetError> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
    if let Some(dir) = dir {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = Path::new(&tmp);
    let _ = fs::remove_file(tmp);
    {
        let mut file = create_owner_only(tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    restrict(tmp)?;
    fs::rename(tmp, path)?;
    restrict(path)
}

/// Writes a non-secret file atomically: next to its final name, then renamed into place.
pub fn write_public(path: &Path, bytes: &[u8]) -> Result<(), NetError> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = Path::new(&tmp);
    {
        let mut file = fs::File::create(tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(tmp, path)?;
    Ok(())
}

/// Reads a secret file, refusing one that other users can read (Unix: any group or other
/// permission bit; Windows: any allowed DACL entry other than the current user, SYSTEM and
/// Administrators).
pub fn read_private(path: &Path) -> Result<Vec<u8>, NetError> {
    check_private(path)?;
    Ok(fs::read(path)?)
}

#[cfg(unix)]
fn create_owner_only(path: &Path) -> Result<fs::File, NetError> {
    use std::os::unix::fs::OpenOptionsExt;
    Ok(fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?)
}

#[cfg(not(unix))]
fn create_owner_only(path: &Path) -> Result<fs::File, NetError> {
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    restrict(path)?;
    Ok(file)
}

#[cfg(unix)]
fn restrict(path: &Path) -> Result<(), NetError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(windows)]
fn restrict(path: &Path) -> Result<(), NetError> {
    crate::acl_windows::restrict_to_owner(path)
}

#[cfg(not(any(unix, windows)))]
fn restrict(_path: &Path) -> Result<(), NetError> {
    Ok(())
}

#[cfg(unix)]
fn check_private(path: &Path) -> Result<(), NetError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)?.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(NetError::InsecureFile(path.display().to_string()));
    }
    Ok(())
}

#[cfg(windows)]
fn check_private(path: &Path) -> Result<(), NetError> {
    fs::metadata(path)?;
    if !crate::acl_windows::owner_only(path)? {
        return Err(NetError::InsecureFile(path.display().to_string()));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn check_private(path: &Path) -> Result<(), NetError> {
    fs::metadata(path)?;
    Ok(())
}
