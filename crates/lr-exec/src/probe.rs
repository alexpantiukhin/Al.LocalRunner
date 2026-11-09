#[cfg(unix)]
use crate::os_unix as os;
#[cfg(windows)]
use crate::os_windows as os;
use lr_abstractions::{Liveness, ProcessInspector};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Probe {
    Alive(u64),
    Gone,
    Unreadable,
}

pub(crate) fn probe(pid: u32) -> Probe {
    os::probe(pid)
}

/// The start fingerprint of a running process (Windows: creation time from `GetProcessTimes`;
/// Linux: start time from `/proc/<pid>/stat` made absolute with the boot time, in ms; macOS:
/// start time from `proc_pidinfo`, in µs); `None` when the process is gone or unreadable.
/// The CLI records it for the submitting process and the runner for its services, so a reused
/// pid is never mistaken for the original process (spec 7.3).
pub fn process_start(pid: u32) -> Option<u64> {
    match probe(pid) {
        Probe::Alive(start) => Some(start),
        _ => None,
    }
}

pub(crate) fn descendants(roots: &[(u32, u64)]) -> Vec<(u32, u64)> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (pid, parent) in os::parents() {
        if pid != parent {
            children.entry(parent).or_default().push(pid);
        }
    }
    let mut seen: HashSet<u32> = roots.iter().map(|(pid, _)| *pid).collect();
    let mut queue: Vec<(u32, u64)> = roots.to_vec();
    let mut found = Vec::new();
    while let Some((parent, parent_start)) = queue.pop() {
        for &child in children.get(&parent).into_iter().flatten() {
            if !seen.insert(child) {
                continue;
            }
            if let Probe::Alive(start) = probe(child) {
                if start >= parent_start {
                    found.push((child, start));
                    queue.push((child, start));
                }
            }
        }
    }
    found
}

/// [`ProcessInspector`] of this machine, by pid and start fingerprint ([`process_start`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct OsProcessInspector;

impl ProcessInspector for OsProcessInspector {
    fn liveness(&self, pid: u32, start: u64) -> Liveness {
        if pid == 0 {
            return Liveness::Unknown;
        }
        match probe(pid) {
            Probe::Alive(s) if s == start => Liveness::Alive,
            Probe::Alive(_) | Probe::Gone => Liveness::Dead,
            Probe::Unreadable => Liveness::Unknown,
        }
    }
}
