use std::ffi::OsString;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Once, OnceLock};
use std::time::{Duration, Instant};
use tokio::process::Command;

const CGROUP_ROOT: &str = "/sys/fs/cgroup";
const UNIT_PREFIX: &str = "lr-";
const BUS_VARS: [&str; 2] = ["DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR"];
const PROBE_LIMIT: Duration = Duration::from_secs(5);

/// A command, the variables it needs on top of the job's, and the scope it creates.
pub(crate) type Wrapped = (Command, Vec<(OsString, OsString)>, Scope);

struct Systemd {
    exe: PathBuf,
    slice: PathBuf,
}

static SYSTEMD: OnceLock<Option<Systemd>> = OnceLock::new();
static BACKGROUND_PROBE: Once = Once::new();
static NEXT: AtomicU64 = AtomicU64::new(1);

fn find_in_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(program))
        .find(|p| p.is_file())
}

/// The directory of cgroup `line`'s v2 entry (`0::/path`), under the cgroup root.
pub(crate) fn cgroup_dir(text: &str) -> Option<PathBuf> {
    let path = text.lines().find_map(|l| l.strip_prefix("0::"))?;
    Some(Path::new(CGROUP_ROOT).join(path.trim().trim_start_matches('/')))
}

fn probe() -> Option<Systemd> {
    if !Path::new(CGROUP_ROOT).join("cgroup.controllers").is_file() {
        return None;
    }
    let exe = find_in_path("systemd-run")?;
    let mut child = std::process::Command::new(&exe)
        .args(["--user", "--scope", "--quiet", "--collect", "--"])
        .args(["/bin/cat", "/proc/self/cgroup"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let until = Instant::now() + PROBE_LIMIT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut text = String::new();
    child.stdout.take()?.read_to_string(&mut text).ok()?;
    if !status.success() {
        return None;
    }
    let scope = cgroup_dir(&text)?;
    let name = scope.file_name()?.to_str()?;
    if !name.ends_with(".scope") {
        return None;
    }
    Some(Systemd {
        slice: scope.parent()?.to_path_buf(),
        exe,
    })
}

fn systemd_blocking() -> Option<&'static Systemd> {
    SYSTEMD.get_or_init(probe).as_ref()
}

/// The probe's answer for a job about to start, without stalling a tokio worker: a
/// multi-threaded runtime hands the worker's tasks over while the probe runs; on a
/// single-threaded runtime the probe goes to its own thread and this job uses the process
/// group.
fn systemd_for_job() -> Option<&'static Systemd> {
    if let Some(known) = SYSTEMD.get() {
        return known.as_ref();
    }
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current().map(|h| h.runtime_flavor()) {
        Ok(RuntimeFlavor::MultiThread) => tokio::task::block_in_place(systemd_blocking),
        Ok(_) => {
            BACKGROUND_PROBE.call_once(|| {
                std::thread::spawn(|| {
                    systemd_blocking();
                });
            });
            None
        }
        Err(_) => systemd_blocking(),
    }
}

/// The runner pid in a scope name `lr-<pid>-<n>.scope`.
pub(crate) fn owner_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(UNIT_PREFIX)?.strip_suffix(".scope")?;
    let (pid, n) = rest.split_once('-')?;
    n.parse::<u64>().ok()?;
    pid.parse().ok()
}

/// Kills the `lr-<pid>-<n>` scopes of this user whose runner process is gone: what a crashed
/// runner left behind, detached descendants included (spec 3.11). Scopes of live runner
/// processes are left alone.
pub(crate) fn sweep_orphans() {
    let Some(systemd) = systemd_blocking() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&systemd.slice) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(owner_pid) else {
            continue;
        };
        if pid == std::process::id() || crate::probe::probe(pid) != crate::probe::Probe::Gone {
            continue;
        }
        Scope { dir: entry.path() }.signal(libc::SIGKILL);
    }
}

/// Whether job trees go into their own cgroup v2 scope (`systemd-run --user --scope`).
pub(crate) fn available() -> bool {
    systemd_blocking().is_some()
}

fn procs(dir: &Path) -> io::Result<Vec<u32>> {
    let text = std::fs::read_to_string(dir.join("cgroup.procs"))?;
    Ok(text.lines().filter_map(|l| l.trim().parse().ok()).collect())
}

/// Whether a job sets a variable `systemd-run` itself needs: it would reach the job's manager
/// instead of ours, so such a job runs in a process group instead.
pub(crate) fn job_owns_bus(job_vars: &[&str]) -> bool {
    job_vars
        .iter()
        .any(|v| BUS_VARS.iter().any(|b| v.eq_ignore_ascii_case(b)))
}

/// The transient cgroup v2 scope a job tree runs in: everything started in it, detached
/// descendants included, stays in it and is killed with it (spec 3.11, 10). It is addressed by
/// its unit name, so it stays reachable after the root process is gone.
pub(crate) struct Scope {
    dir: PathBuf,
}

impl Scope {
    /// `sh -c command` inside a new scope, or `None` when scopes are unavailable here or the job
    /// sets the bus variables itself. Those variables are added for `systemd-run` and removed
    /// again before the shell.
    pub(crate) fn wrap(command: &str, job_vars: &[&str]) -> Option<Wrapped> {
        if job_owns_bus(job_vars) {
            return None;
        }
        let systemd = systemd_for_job()?;
        let unit = format!(
            "{UNIT_PREFIX}{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let mut cmd = Command::new(&systemd.exe);
        cmd.args(["--user", "--scope", "--quiet", "--collect"])
            .arg(format!("--unit={unit}"))
            .args(["--", "/usr/bin/env"]);
        let mut extra = Vec::new();
        for var in BUS_VARS {
            if let Some(value) = std::env::var_os(var) {
                cmd.args(["-u", var]);
                extra.push((OsString::from(var), value));
            }
        }
        cmd.args(["/bin/sh", "-c", command]);
        let dir = systemd.slice.join(format!("{unit}.scope"));
        Some((cmd, extra, Scope { dir }))
    }

    /// The scope a process started by an earlier runner lives in, when it is one of ours.
    pub(crate) fn of_pid(pid: u32) -> Option<Scope> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
        let dir = cgroup_dir(&text)?;
        let name = dir.file_name()?.to_str()?;
        let unit = name.strip_suffix(".scope")?;
        if !unit.starts_with(UNIT_PREFIX) {
            return None;
        }
        Some(Scope { dir })
    }

    /// Sends `sig` to every process of the scope; `SIGKILL` goes through `cgroup.kill` where
    /// the kernel has it, else through a frozen scope.
    pub(crate) fn signal(&self, sig: libc::c_int) {
        let dir = &self.dir;
        if sig == libc::SIGKILL && std::fs::write(dir.join("cgroup.kill"), "1").is_ok() {
            return;
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
    }

    /// Whether the scope has no process left (a scope that is gone or never came up has
    /// none); `None` when its processes cannot be read.
    pub(crate) fn is_empty(&self) -> Option<bool> {
        match procs(&self.dir) {
            Ok(p) => Some(p.is_empty()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Some(true),
            Err(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_v2_line_names_the_scope_directory() {
        let text = "1:name=systemd:/x\n0::/user.slice/user-1000.slice/user@1000.service/app.slice/lr-7-1.scope\n";
        assert_eq!(
            cgroup_dir(text).unwrap(),
            Path::new("/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/lr-7-1.scope")
        );
        assert_eq!(cgroup_dir("1:cpu:/x\n"), None);
    }

    #[test]
    fn the_runner_pid_is_read_from_our_scope_names_only() {
        assert_eq!(owner_pid("lr-4242-7.scope"), Some(4242));
        assert_eq!(owner_pid("lr-4242.scope"), None);
        assert_eq!(owner_pid("lr-4242-x.scope"), None);
        assert_eq!(owner_pid("app-4242-7.scope"), None);
        assert_eq!(owner_pid("lr-4242-7.service"), None);
    }

    #[test]
    fn a_job_setting_bus_variables_owns_them() {
        assert!(job_owns_bus(&["PATH", "xdg_runtime_dir"]));
        assert!(job_owns_bus(&["DBUS_SESSION_BUS_ADDRESS"]));
        assert!(!job_owns_bus(&["PATH", "HOME"]));
    }

    #[test]
    fn a_scope_that_never_came_up_is_empty() {
        let scope = Scope {
            dir: PathBuf::from("/sys/fs/cgroup/no-such-lr-scope.scope"),
        };
        assert_eq!(scope.is_empty(), Some(true));
    }
}
