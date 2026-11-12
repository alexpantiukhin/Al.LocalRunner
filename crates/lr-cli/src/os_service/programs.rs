//! System programs by absolute path: a bare name would be looked up in the directory of this
//! binary first on Windows (a directory its user can write), which an elevated run must never
//! do; on Unix `PATH` is the caller's.

use std::path::{Path, PathBuf};

/// Directories searched on Unix, in order.
pub const UNIX_DIRS: [&str; 4] = ["/usr/sbin", "/usr/bin", "/sbin", "/bin"];

/// The Windows program `name` in the system directory `system` (`System32`); `powershell` is
/// Windows PowerShell under it; a name without extension gets `.exe`.
pub fn windows_program(system: &Path, name: &str) -> PathBuf {
    let bare = name.strip_suffix(".exe").unwrap_or(name);
    if bare.eq_ignore_ascii_case("powershell") {
        return system
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
    }
    system.join(format!("{bare}.exe"))
}

/// The Unix program `name` in the first of [`UNIX_DIRS`] that has it.
pub fn unix_program(name: &str, exists: impl Fn(&Path) -> bool) -> Result<PathBuf, String> {
    UNIX_DIRS
        .iter()
        .map(|d| Path::new(d).join(name))
        .find(|p| exists(p))
        .ok_or_else(|| format!("{name} is not in {}", UNIX_DIRS.join(", ")))
}

#[cfg(windows)]
fn system_directory() -> Result<PathBuf, String> {
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut buf = vec![0u16; 260];
    let len = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if len == 0 || len >= buf.len() {
        return Err(format!(
            "GetSystemDirectory: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
}

/// The absolute path of system program `name` (an absolute `name` stays as it is).
pub fn program_path(name: &str) -> Result<PathBuf, String> {
    if Path::new(name).is_absolute() {
        return Ok(PathBuf::from(name));
    }
    #[cfg(windows)]
    {
        Ok(windows_program(&system_directory()?, name))
    }
    #[cfg(not(windows))]
    {
        unix_program(name, |p| p.is_file())
    }
}
