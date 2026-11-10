//! Facts about this machine the assembly passes to the roles: memory, cores, free disk,
//! elevation, container, and the installation's MachineId.

use lr_broker::policy::HostResources;
use std::path::Path;

/// File of the installation's MachineId in the state directory (spec 10.3).
pub const MACHINE_ID_FILE: &str = "machine-id";

/// Total memory in MB and logical cores of this host.
pub fn resources() -> HostResources {
    let cpu = std::thread::available_parallelism()
        .map(|n| u32::try_from(n.get()).unwrap_or(u32::MAX))
        .unwrap_or(1);
    HostResources {
        mem_mb: total_memory_mb().unwrap_or(0),
        cpu,
    }
}

#[cfg(windows)]
fn total_memory_mb() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.ullTotalPhys / (1 << 20))
}

#[cfg(target_os = "linux")]
fn total_memory_mb() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn total_memory_mb() -> Option<u64> {
    let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    (pages > 0 && size > 0).then(|| (pages as u64).saturating_mul(size as u64) / (1 << 20))
}

#[cfg(not(any(windows, unix)))]
fn total_memory_mb() -> Option<u64> {
    None
}

/// Free space in MB on the volume of `path` (the nearest existing ancestor).
pub fn free_disk_mb(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    free_bytes(existing).map(|b| b / (1 << 20))
}

#[cfg(windows)]
fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

#[cfg(unix)]
fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)]
    Some((s.f_bavail as u64).saturating_mul(s.f_frsize as u64))
}

#[cfg(not(any(windows, unix)))]
fn free_bytes(_: &Path) -> Option<u64> {
    None
}

/// Whether this process runs as root, an elevated administrator or `SYSTEM` (spec 3.12).
#[cfg(windows)]
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut len = 0u32;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    };
    unsafe { CloseHandle(token) };
    let system = lr_net::current_os_principal().is_ok_and(|sid| sid == "S-1-5-18");
    system || (ok != 0 && elevation.TokenIsElevated != 0)
}

/// Whether this process runs as root, an elevated administrator or `SYSTEM` (spec 3.12).
#[cfg(unix)]
pub fn is_elevated() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// Whether this process runs as root, an elevated administrator or `SYSTEM` (spec 3.12).
#[cfg(not(any(windows, unix)))]
pub fn is_elevated() -> bool {
    false
}

/// Whether the process runs inside a container (Docker, Podman), where alone
/// `[listen] address = "container"` is accepted (spec 10.4).
pub fn in_container() -> bool {
    cfg!(target_os = "linux")
        && (Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists())
}

/// The installation's MachineId: read from `machine-id` in `root`, created on first use.
pub fn machine_id(root: &Path) -> Result<String, String> {
    let path = root.join(MACHINE_ID_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let id = text.trim().to_string();
        if !id.is_empty() {
            return Ok(id);
        }
    }
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("no randomness for machine-id: {e}"))?;
    let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    std::fs::write(&path, &id).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(id)
}
