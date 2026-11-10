use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::process::Command;

const CGROUP_ROOT: &str = "/sys/fs/cgroup";
const UNIT_PREFIX: &str = "lr-";
const BUS_VARS: [&str; 2] = ["DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR"];
const PROBE_LIMIT: Duration = Duration::from_secs(5);

/// A command, the variables it needs on top of the job's, and the scope it creates.
pub(crate) type Wrapped = (Command, Vec<(OsString, OsString)>, Scope);

static SYSTEMD_RUN: OnceLock<Option<PathBuf>> = OnceLock::new();
static NEXT: AtomicU64 = AtomicU64::new(1);

fn find_in_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(program))
        .find(|p| p.is_file())
}

fn probe_systemd_run() -> Option<PathBuf> {
    if !Path::new(CGROUP_ROOT).join("cgroup.controllers").is_file() {
        return None;
    }
    let exe = find_in_path("systemd-run")?;
    let mut child = std::process::Command::new(&exe)
        .args(["--user", "--scope", "--quiet", "--collect", "--"])
        .args(["/bin/sh", "-c", ":"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let until = Instant::now() + PROBE_LIMIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success().then_some(exe),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn systemd_run() -> Option<&'static Path> {
    SYSTEMD_RUN.get_or_init(probe_systemd_run).as_deref()
}

/// Whether job trees go into their own cgroup v2 scope (`systemd-run --user --scope`).
pub(crate) fn available() -> bool {
    systemd_run().is_some()
}

fn cgroup_of(pid: u32) -> Option<PathBuf> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let path = text.lines().find_map(|l| l.strip_prefix("0::"))?;
    Some(Path::new(CGROUP_ROOT).join(path.trim().trim_start_matches('/')))
}

fn procs(dir: &Path) -> io::Result<Vec<u32>> {
    let text = std::fs::read_to_string(dir.join("cgroup.procs"))?;
    Ok(text.lines().filter_map(|l| l.trim().parse().ok()).collect())
}

/// The transient cgroup v2 scope a job tree runs in: everything started in it, detached
/// descendants included, stays in it and is killed with it (spec 3.11, 10).
pub(crate) struct Scope {
    unit: String,
    dir: OnceLock<PathBuf>,
}

impl Scope {
    /// `sh -c command` inside a new scope, or `None` when scopes are unavailable here. The bus
    /// variables `systemd-run` needs are added for it and removed again before the shell, unless
    /// the job sets them itself.
    pub(crate) fn wrap(command: &str, job_vars: &[&str]) -> Option<Wrapped> {
        let exe = systemd_run()?;
        let unit = format!(
            "{UNIT_PREFIX}{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let mut cmd = Command::new(exe);
        cmd.args(["--user", "--scope", "--quiet", "--collect"])
            .arg(format!("--unit={unit}"))
            .args(["--", "/usr/bin/env"]);
        let mut extra = Vec::new();
        for var in BUS_VARS {
            if job_vars.iter().any(|v| v.eq_ignore_ascii_case(var)) {
                continue;
            }
            if let Some(value) = std::env::var_os(var) {
                cmd.args(["-u", var]);
                extra.push((OsString::from(var), value));
            }
        }
        cmd.args(["/bin/sh", "-c", command]);
        let scope = Scope {
            unit,
            dir: OnceLock::new(),
        };
        Some((cmd, extra, scope))
    }

    /// The scope a process started by an earlier runner lives in, when it is one of ours.
    pub(crate) fn of_pid(pid: u32) -> Option<Scope> {
        let dir = cgroup_of(pid)?;
        let name = dir.file_name()?.to_str()?;
        let unit = name.strip_suffix(".scope")?;
        if !unit.starts_with(UNIT_PREFIX) {
            return None;
        }
        let scope = Scope {
            unit: unit.to_string(),
            dir: OnceLock::new(),
        };
        let _ = scope.dir.set(dir);
        Some(scope)
    }

    fn resolve(&self, root: u32) -> Option<&Path> {
        if let Some(dir) = self.dir.get() {
            return Some(dir);
        }
        let dir = cgroup_of(root)?;
        let ours = dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.strip_suffix(".scope") == Some(self.unit.as_str()));
        if !ours {
            return None;
        }
        Some(self.dir.get_or_init(|| dir))
    }

    /// Remembers the scope's directory once the root process is inside it.
    pub(crate) fn observe(&self, root: u32) {
        let _ = self.resolve(root);
    }

    /// Sends `sig` to every process of the scope; `SIGKILL` goes through `cgroup.kill` where
    /// the kernel has it, else through a frozen scope. `false` while the scope is not known.
    pub(crate) fn signal(&self, root: u32, sig: libc::c_int) -> bool {
        let Some(dir) = self.resolve(root) else {
            return false;
        };
        if sig == libc::SIGKILL && std::fs::write(dir.join("cgroup.kill"), "1").is_ok() {
            return true;
        }
        let freeze = sig == libc::SIGKILL && std::fs::write(dir.join("cgroup.freeze"), "1").is_ok();
        for pid in procs(dir).unwrap_or_default() {
            if pid > 0 && pid <= i32::MAX as u32 {
                unsafe {
                    libc::kill(pid as libc::pid_t, sig);
                }
            }
        }
        if freeze {
            let _ = std::fs::write(dir.join("cgroup.freeze"), "0");
        }
        true
    }

    /// Whether the scope has no process left; `None` while the scope is not known.
    pub(crate) fn is_empty(&self, root: u32) -> Option<bool> {
        let dir = self.resolve(root)?;
        Some(match procs(dir) {
            Ok(p) => p.is_empty(),
            Err(e) => e.kind() == io::ErrorKind::NotFound,
        })
    }
}
