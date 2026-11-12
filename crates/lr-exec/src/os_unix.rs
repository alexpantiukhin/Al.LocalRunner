use crate::probe::Probe;
use std::collections::HashMap;

pub(crate) fn signal(pid: u32, start: u64, signal: libc::c_int) {
    if pid == 0 || pid > i32::MAX as u32 {
        return;
    }
    if probe(pid) == Probe::Alive(start) {
        unsafe {
            libc::kill(pid as libc::pid_t, signal);
        }
    }
}

/// Kills process group `pgid` whose leader is gone: a pid is not handed out again while a
/// process group with that id exists, so a group named after a vanished root is that root's.
pub(crate) fn kill_left_group(pgid: u32) {
    if pgid <= 1 || pgid > i32::MAX as u32 || probe(pgid) != Probe::Gone {
        return;
    }
    unsafe {
        libc::killpg(pgid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod imp {
    use super::Probe;
    use std::collections::HashMap;
    use std::sync::OnceLock;

    struct Stat {
        state: char,
        ppid: u32,
        start_ticks: u64,
    }

    fn parse(text: &str) -> Option<Stat> {
        let rest = &text[text.rfind(')')? + 1..];
        let fields: Vec<&str> = rest.split_whitespace().collect();
        Some(Stat {
            state: fields.first()?.chars().next()?,
            ppid: fields.get(1)?.parse().ok()?,
            start_ticks: fields.get(19)?.parse().ok()?,
        })
    }

    const TICK_BITS: u32 = 40;

    fn boot_tag() -> u64 {
        static CACHE: OnceLock<u64> = OnceLock::new();
        *CACHE.get_or_init(|| {
            let id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
            let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
            for byte in id.trim().bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
            hash >> TICK_BITS
        })
    }

    pub(crate) fn probe(pid: u32) -> Probe {
        if pid == 0 {
            return Probe::Unreadable;
        }
        let text = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Probe::Gone,
            Err(_) => return Probe::Unreadable,
        };
        let Some(stat) = parse(&text) else {
            return Probe::Unreadable;
        };
        if stat.state == 'Z' || stat.state == 'X' {
            return Probe::Gone;
        }
        Probe::Alive((boot_tag() << TICK_BITS) | (stat.start_ticks & ((1 << TICK_BITS) - 1)))
    }

    pub(crate) fn parents() -> HashMap<u32, u32> {
        let mut map = HashMap::new();
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return map;
        };
        for entry in dir.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            if let Some(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .as_deref()
                .and_then(parse)
            {
                map.insert(pid, stat.ppid);
            }
        }
        map
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Probe;
    use std::collections::HashMap;
    use std::mem::{size_of, zeroed};

    const SZOMB: u32 = 5;

    fn info(pid: u32) -> Result<libc::proc_bsdinfo, Probe> {
        let mut info: libc::proc_bsdinfo = unsafe { zeroed() };
        let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let got = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut libc::proc_bsdinfo as *mut libc::c_void,
                size,
            )
        };
        if got == size {
            return Ok(info);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => Err(Probe::Gone),
            _ => Err(Probe::Unreadable),
        }
    }

    pub(crate) fn probe(pid: u32) -> Probe {
        if pid == 0 || pid > i32::MAX as u32 {
            return Probe::Unreadable;
        }
        match info(pid) {
            Ok(info) if info.pbi_status == SZOMB => Probe::Gone,
            Ok(info) => Probe::Alive(info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec),
            Err(probe) => probe,
        }
    }

    pub(crate) fn parents() -> HashMap<u32, u32> {
        let mut map = HashMap::new();
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if count <= 0 {
            return map;
        }
        let mut pids = vec![0 as libc::pid_t; count as usize * 2 + 64];
        let bytes = (pids.len() * size_of::<libc::pid_t>()) as libc::c_int;
        let got = unsafe { libc::proc_listallpids(pids.as_mut_ptr() as *mut libc::c_void, bytes) };
        if got <= 0 {
            return map;
        }
        for &pid in pids.iter().take(got as usize) {
            if pid > 0 {
                if let Ok(info) = info(pid as u32) {
                    map.insert(pid as u32, info.pbi_ppid);
                }
            }
        }
        map
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
mod imp {
    use super::Probe;
    use std::collections::HashMap;

    pub(crate) fn probe(_pid: u32) -> Probe {
        Probe::Unreadable
    }

    pub(crate) fn parents() -> HashMap<u32, u32> {
        HashMap::new()
    }
}

pub(crate) fn probe(pid: u32) -> Probe {
    imp::probe(pid)
}

pub(crate) fn parents() -> HashMap<u32, u32> {
    imp::parents()
}
