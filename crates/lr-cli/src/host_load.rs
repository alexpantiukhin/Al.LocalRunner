//! [`lr_abstractions::HostLoad`] of this machine: free memory and CPU busy share from the OS.
//! Windows reads `GlobalMemoryStatusEx` and `GetSystemTimes`, Linux `/proc/meminfo` and
//! `/proc/stat`; other systems measure nothing, so their runners send no load and are admitted
//! as before.

use lr_abstractions::load::CpuTimes;
use lr_abstractions::{HostLoad, LoadSample};
use std::sync::Mutex;

/// Reads the load of the machine this process runs on. The CPU share is measured between two
/// consecutive readings, so the first one carries no load.
pub struct SystemHostLoad {
    previous: Mutex<Option<CpuTimes>>,
}

impl SystemHostLoad {
    /// A reader that has not read anything yet.
    pub fn new() -> SystemHostLoad {
        SystemHostLoad {
            previous: Mutex::new(None),
        }
    }
}

impl Default for SystemHostLoad {
    fn default() -> Self {
        SystemHostLoad::new()
    }
}

impl HostLoad for SystemHostLoad {
    fn sample(&self) -> Option<LoadSample> {
        let free_mem_mb = free_memory_mb()?;
        let now = cpu_times()?;
        let prev = self
            .previous
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .replace(now)?;
        let cores = std::thread::available_parallelism()
            .map(|n| u32::try_from(n.get()).unwrap_or(u32::MAX))
            .unwrap_or(1);
        Some(LoadSample {
            free_mem_mb,
            cpu_busy_percent: lr_abstractions::load::busy_percent(prev, now)?,
            cpu_cores: cores,
        })
    }
}

#[cfg(windows)]
fn free_memory_mb() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.ullAvailPhys / (1 << 20))
}

#[cfg(windows)]
fn cpu_times() -> Option<CpuTimes> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetSystemTimes;
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut idle, mut kernel, mut user) = (zero, zero, zero);
    if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
        return None;
    }
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    Some(CpuTimes {
        idle: ticks(idle),
        total: ticks(kernel).saturating_add(ticks(user)),
    })
}

#[cfg(target_os = "linux")]
fn free_memory_mb() -> Option<u64> {
    lr_abstractions::load::mem_available_mb(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

#[cfg(target_os = "linux")]
fn cpu_times() -> Option<CpuTimes> {
    lr_abstractions::load::proc_stat_times(&std::fs::read_to_string("/proc/stat").ok()?)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn free_memory_mb() -> Option<u64> {
    None
}

#[cfg(not(any(windows, target_os = "linux")))]
fn cpu_times() -> Option<CpuTimes> {
    None
}
