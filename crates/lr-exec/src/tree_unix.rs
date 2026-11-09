use crate::guard::Reaped;
use crate::os_unix::signal;
use crate::probe::{descendants, probe, Probe};
use std::collections::HashMap;
use std::io;
use std::sync::Mutex;
use tokio::process::{Child, Command};

pub(crate) struct Tree {
    pgid: i32,
    root: u32,
    start: Option<u64>,
    group_gone: Reaped,
    known: Mutex<HashMap<u32, u64>>,
    owned: bool,
}

impl Tree {
    pub(crate) fn prepare(cmd: &mut Command) {
        cmd.process_group(0);
    }

    pub(crate) fn attach(child: &Child) -> io::Result<Tree> {
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("process already exited"))?;
        let start = match probe(pid) {
            Probe::Alive(start) => Some(start),
            _ => None,
        };
        Ok(Tree::new(pid, start, true, true))
    }

    pub(crate) fn adopt(pid: u32, start: u64) -> Tree {
        let leader = pid <= i32::MAX as u32
            && unsafe { libc::getpgid(pid as libc::pid_t) } == pid as libc::pid_t;
        Tree::new(pid, Some(start), leader, false)
    }

    fn new(pid: u32, start: Option<u64>, leader: bool, owned: bool) -> Tree {
        let group_gone = Reaped::default();
        if !leader {
            group_gone.mark();
        }
        let known = start.map(|s| (pid, s)).into_iter().collect();
        Tree {
            pgid: if leader { pid as i32 } else { 0 },
            root: pid,
            start,
            group_gone,
            known: Mutex::new(known),
            owned,
        }
    }

    pub(crate) fn start(&self) -> Option<u64> {
        self.start
    }

    pub(crate) fn observe(&self) {
        let mut known = self.known.lock().unwrap_or_else(|e| e.into_inner());
        let mut roots: Vec<(u32, u64)> = known.iter().map(|(p, s)| (*p, *s)).collect();
        if let Some(start) = self.start {
            roots.push((self.root, start));
        }
        for (pid, start) in descendants(&roots) {
            known.insert(pid, start);
        }
    }

    fn signal_all(&self, sig: libc::c_int) {
        self.observe();
        if self.pgid > 0 {
            self.group_gone.run_if_alive(|| unsafe {
                libc::killpg(self.pgid, sig);
            });
        }
        let known = self.known.lock().unwrap_or_else(|e| e.into_inner());
        for (pid, start) in known.iter() {
            signal(*pid, *start, sig);
        }
    }

    pub(crate) fn soft_stop(&self) {
        self.signal_all(libc::SIGTERM);
    }

    pub(crate) fn kill(&self) {
        self.signal_all(libc::SIGKILL);
    }

    pub(crate) fn is_empty(&self) -> bool {
        if !self.group_gone.is_marked() {
            let gone = self.pgid <= 0
                || unsafe {
                    libc::killpg(self.pgid, 0) == -1
                        && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                };
            if gone {
                self.group_gone.mark();
            }
        }
        let mut known = self.known.lock().unwrap_or_else(|e| e.into_inner());
        known.retain(|pid, start| probe(*pid) == Probe::Alive(*start));
        (self.group_gone.is_marked() || !self.owned) && known.is_empty()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        self.kill();
    }
}
