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

    fn boot_and_tick() -> Option<(u64, u64)> {
        static CACHE: OnceLock<Option<(u64, u64)>> = OnceLock::new();
        *CACHE.get_or_init(|| {
            let stat = std::fs::read_to_string("/proc/stat").ok()?;
            let btime = stat
                .lines()
                .find_map(|l| l.strip_prefix("btime "))?
                .trim()
                .parse()
                .ok()?;
            let tick = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
            if tick <= 0 {
                return None;
            }
            Some((btime, tick as u64))
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
        let (Some(stat), Some((btime, tick))) = (parse(&text), boot_and_tick()) else {
            return Probe::Unreadable;
        };
        if stat.state == 'Z' || stat.state == 'X' {
            return Probe::Gone;
        }
        Probe::Alive(btime * 1000 + stat.start_ticks * 1000 / tick)
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
