use crate::guard::Reaped;
use crate::os_unix::signal;
use crate::probe::{descendants, probe, Probe};
#[cfg(target_os = "linux")]
use crate::scope_linux::Scope;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::sync::Mutex;
use tokio::process::{Child, Command};

#[cfg(not(target_os = "linux"))]
struct Scope;

#[cfg(not(target_os = "linux"))]
impl Scope {
    fn wrap(_: &str, _: &[&str]) -> Option<(Command, Vec<(OsString, OsString)>, Scope)> {
        None
    }

    fn of_pid(_: u32) -> Option<Scope> {
        None
    }

    fn signal(&self, _: libc::c_int) {}

    fn is_empty(&self) -> Option<bool> {
        None
    }
}

pub(crate) struct Placement(Option<Scope>);

pub(crate) struct Tree {
    scope: Option<Scope>,
    pgid: i32,
    root: u32,
    start: Option<u64>,
    group_gone: Reaped,
    known: Mutex<HashMap<u32, u64>>,
    owned: bool,
}

impl Tree {
    pub(crate) fn shell(
        command: &str,
        job_vars: &[&str],
    ) -> (Command, Vec<(OsString, OsString)>, Placement) {
        if let Some((cmd, extra, scope)) = Scope::wrap(command, job_vars) {
            return (cmd, extra, Placement(Some(scope)));
        }
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(command);
        (cmd, Vec::new(), Placement(None))
    }

    pub(crate) fn prepare(cmd: &mut Command) {
        cmd.process_group(0);
    }

    pub(crate) fn attach(child: &Child, placement: Placement) -> io::Result<Tree> {
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("process already exited"))?;
        let start = match probe(pid) {
            Probe::Alive(start) => Some(start),
            _ => None,
        };
        let mut tree = Tree::new(pid, start, true, true);
        tree.scope = placement.0;
        Ok(tree)
    }

    pub(crate) fn adopt(pid: u32, start: u64) -> Tree {
        let leader = pid <= i32::MAX as u32
            && unsafe { libc::getpgid(pid as libc::pid_t) } == pid as libc::pid_t;
        let mut tree = Tree::new(pid, Some(start), leader, false);
        tree.scope = Scope::of_pid(pid);
        tree
    }

    fn new(pid: u32, start: Option<u64>, leader: bool, owned: bool) -> Tree {
        let group_gone = Reaped::default();
        if !leader {
            group_gone.mark();
        }
        let known = start.map(|s| (pid, s)).into_iter().collect();
        Tree {
            scope: None,
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
        if let Some(scope) = &self.scope {
            scope.signal(sig);
        }
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

    pub(crate) fn soft_stop(&self) -> bool {
        self.signal_all(libc::SIGTERM);
        true
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
        let scope_empty = self
            .scope
            .as_ref()
            .map_or(Some(true), |s| s.is_empty())
            .unwrap_or(false);
        scope_empty && (self.group_gone.is_marked() || !self.owned) && known.is_empty()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        self.kill();
    }
}
