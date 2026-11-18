use lr_abstractions::ResourceLimits;
use std::ffi::{CStr, CString, OsString};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::{Duration, Instant};
use tokio::process::Command;

use crate::limits::{MemoryEvidence, MemoryHold};

const CGROUP_ROOT: &str = "/sys/fs/cgroup";
const UNIT_PREFIX: &str = "lr-";
const BUS_VARS: [&str; 2] = ["DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR"];
const PROBE_LIMIT: Duration = Duration::from_secs(5);
const LEAF: &str = "runner";
const OPTIONAL_CONTROLLERS: [&str; 2] = ["cpu", "pids"];
const CPU_PERIOD_US: u64 = 100_000;

/// A command, the variables it needs on top of the job's, and the scope it creates.
pub(crate) type Wrapped = (Command, Vec<(OsString, OsString)>, Scope);

type Cleanup = fn(&Path) -> io::Result<()>;

struct Systemd {
    exe: PathBuf,
    slice: PathBuf,
    controllers: Vec<String>,
}

/// A cgroup v2 subtree handed to the runner process (a system service with `Delegate=yes`):
/// the runner lives in its `runner` leaf and makes one child cgroup per job tree.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Delegated {
    base: PathBuf,
    controllers: Vec<String>,
}

enum Backend {
    UserManager(Systemd),
    Delegated(Delegated),
}

/// Which cgroup v2 mechanism holds job trees here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    UserScope,
    Delegated,
}

static BACKEND: OnceLock<Option<Backend>> = OnceLock::new();
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
    cgroup_dir_in(Path::new(CGROUP_ROOT), text)
}

fn cgroup_dir_in(root: &Path, text: &str) -> Option<PathBuf> {
    let path = text.lines().find_map(|l| l.strip_prefix("0::"))?;
    Some(root.join(path.trim().trim_start_matches('/')))
}

fn words(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|t| {
            t.split_whitespace()
                .map(|w| w.trim_start_matches('+').to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn has(list: &[String], name: &str) -> bool {
    list.iter().any(|c| c == name)
}

fn probe_user_manager() -> Option<Systemd> {
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
    let slice = scope.parent()?.to_path_buf();
    let controllers = words(&slice.join("cgroup.controllers"));
    Some(Systemd {
        slice,
        exe,
        controllers,
    })
}

fn probe() -> Option<Backend> {
    let root = Path::new(CGROUP_ROOT);
    if !root.join("cgroup.controllers").is_file() {
        return None;
    }
    if let Some(systemd) = probe_user_manager() {
        return Some(Backend::UserManager(systemd));
    }
    let text = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    delegate(root, &text, std::process::id()).map(Backend::Delegated)
}

/// One change to the cgroup tree.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Mkdir(PathBuf),
    Write(PathBuf, String),
}

/// What makes the cgroup `base` of process `pid` a parent of job cgroups, in order: the leaf
/// `runner`, the process moved into it (a cgroup with processes cannot enable controllers for
/// its children), then the memory controller enabled for the children.
pub(crate) fn setup_steps(base: &Path, pid: u32) -> Vec<Step> {
    let leaf = base.join(LEAF);
    vec![
        Step::Mkdir(leaf.clone()),
        Step::Write(leaf.join("cgroup.procs"), pid.to_string()),
        Step::Write(base.join("cgroup.subtree_control"), "+memory".to_string()),
    ]
}

fn run_step(step: &Step) -> io::Result<()> {
    match step {
        Step::Mkdir(dir) => match std::fs::create_dir(dir) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            other => other,
        },
        Step::Write(path, text) => std::fs::write(path, text),
    }
}

fn writable(dir: &Path) -> bool {
    CString::new(dir.as_os_str().as_bytes())
        .is_ok_and(|c| unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0)
}

fn below(root: &Path, dir: &Path) -> bool {
    dir.strip_prefix(root)
        .is_ok_and(|rest| rest.components().next().is_some())
}

/// The delegated subtree of process `pid` whose `/proc/self/cgroup` is `self_cgroup`, under
/// the cgroup root `root`: its own cgroup when it is not the root, holds the memory controller
/// and no other process, and the process can enable the controller for children after moving
/// itself into the `runner` leaf ([`setup_steps`]); a process already in such a leaf keeps it.
/// The kernel must have `cgroup.kill` (Linux 5.14+), so a job tree is killed whole even while
/// it forks. `None`, with nothing changed, when any of this does not hold.
pub(crate) fn delegate(root: &Path, self_cgroup: &str, pid: u32) -> Option<Delegated> {
    let own = cgroup_dir_in(root, self_cgroup)?;
    if !below(root, &own) || !own.join("cgroup.kill").is_file() {
        return None;
    }
    if let Some(parent) = own.parent().filter(|p| below(root, p)) {
        let enabled = words(&parent.join("cgroup.subtree_control"));
        if own.file_name().is_some_and(|n| n == LEAF) && has(&enabled, "memory") && writable(parent)
        {
            return Some(Delegated {
                base: parent.to_path_buf(),
                controllers: enabled,
            });
        }
    }
    let available = words(&own.join("cgroup.controllers"));
    if !has(&available, "memory") || procs_here(&own).ok()? != [pid] {
        return None;
    }
    let steps = setup_steps(&own, pid);
    for (done, step) in steps.iter().enumerate() {
        if run_step(step).is_err() {
            if done >= 2 {
                let _ = std::fs::write(own.join("cgroup.procs"), pid.to_string());
            }
            if done >= 1 {
                let _ = std::fs::remove_dir(own.join(LEAF));
            }
            return None;
        }
    }
    let mut controllers = vec!["memory".to_string()];
    for c in OPTIONAL_CONTROLLERS {
        if has(&available, c)
            && std::fs::write(own.join("cgroup.subtree_control"), format!("+{c}")).is_ok()
        {
            controllers.push(c.to_string());
        }
    }
    Some(Delegated {
        base: own,
        controllers,
    })
}

fn backend_blocking() -> Option<&'static Backend> {
    BACKEND.get_or_init(probe).as_ref()
}

/// The probe's answer for a job about to start, without stalling a tokio worker: a
/// multi-threaded runtime hands the worker's tasks over while the probe runs; on a
/// single-threaded runtime the probe goes to its own thread and this job uses the process
/// group.
fn backend_for_job() -> Option<&'static Backend> {
    if let Some(known) = BACKEND.get() {
        return known.as_ref();
    }
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current().map(|h| h.runtime_flavor()) {
        Ok(RuntimeFlavor::MultiThread) => tokio::task::block_in_place(backend_blocking),
        Ok(_) => {
            BACKGROUND_PROBE.call_once(|| {
                std::thread::spawn(|| {
                    backend_blocking();
                });
            });
            None
        }
        Err(_) => backend_blocking(),
    }
}

/// The runner pid in a job cgroup name: `lr-<pid>-<n>.scope` of the user manager or
/// `lr-<pid>-<n>` of a delegated subtree.
pub(crate) fn owner_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(UNIT_PREFIX)?;
    let rest = rest.strip_suffix(".scope").unwrap_or(rest);
    let (pid, n) = rest.split_once('-')?;
    n.parse::<u64>().ok()?;
    pid.parse().ok()
}

/// Kills the `lr-<pid>-<n>` job cgroups whose runner process is gone: what a crashed runner
/// left behind, detached descendants included (spec 3.11); in a delegated subtree they are
/// removed once empty. Job cgroups of live runner processes are left alone.
pub(crate) fn sweep_orphans() {
    let Some(backend) = backend_blocking() else {
        return;
    };
    let (dir, cleanup): (&Path, Option<Cleanup>) = match backend {
        Backend::UserManager(s) => (s.slice.as_path(), None),
        Backend::Delegated(d) => (d.base.as_path(), Some(remove_cgroup as Cleanup)),
    };
    if let Some(cleanup) = cleanup {
        retry_removal(&mut leftovers(), cleanup);
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(owner_pid) else {
            continue;
        };
        if pid == std::process::id() || crate::probe::probe(pid) != crate::probe::Probe::Gone {
            continue;
        }
        let mut scope = Scope::bare(entry.path());
        scope.cleanup = cleanup;
        scope.signal(libc::SIGKILL);
    }
}

/// Which mechanism holds job trees here, if any.
pub(crate) fn kind() -> Option<Kind> {
    backend_blocking().map(|b| match b {
        Backend::UserManager(_) => Kind::UserScope,
        Backend::Delegated(_) => Kind::Delegated,
    })
}

/// Whether job cgroups get the memory controller, so the OS can limit a job's memory.
pub(crate) fn memory_delegated() -> bool {
    backend_blocking().is_some_and(|b| match b {
        Backend::UserManager(s) => has(&s.controllers, "memory"),
        Backend::Delegated(d) => has(&d.controllers, "memory"),
    })
}

fn procs_here(dir: &Path) -> io::Result<Vec<u32>> {
    let text = std::fs::read_to_string(dir.join("cgroup.procs"))?;
    Ok(text.lines().filter_map(|l| l.trim().parse().ok()).collect())
}

fn procs(dir: &Path) -> io::Result<Vec<u32>> {
    let mut out = procs_here(dir)?;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                out.extend(procs(&entry.path()).unwrap_or_default());
            }
        }
    }
    Ok(out)
}

fn remove_cgroup(dir: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            remove_cgroup(&entry.path())?;
        }
    }
    std::fs::remove_dir(dir)
}

/// Whether a job sets a variable `systemd-run` itself needs: it would reach the job's manager
/// instead of ours, so such a job runs in a process group instead.
pub(crate) fn job_owns_bus(job_vars: &[&str]) -> bool {
    job_vars
        .iter()
        .any(|v| BUS_VARS.iter().any(|b| v.eq_ignore_ascii_case(b)))
}

/// The cgroup v2 interface file and value of a `systemd-run` property, for a cgroup the runner
/// makes itself; `None` for a property without one (`OOMPolicy` is the unit's, and a cgroup
/// without `memory.oom.group` already continues after an OOM kill).
pub(crate) fn cgroup_file(property: &str) -> Option<(&'static str, String)> {
    let (key, value) = property.split_once('=')?;
    match key {
        "MemoryHigh" => Some(("memory.high", value.to_string())),
        "MemoryMax" => Some(("memory.max", value.to_string())),
        "MemorySwapMax" => Some(("memory.swap.max", value.to_string())),
        "CPUQuota" => {
            let percent: u64 = value.strip_suffix('%')?.parse().ok()?;
            Some((
                "cpu.max",
                format!("{} {CPU_PERIOD_US}", percent * CPU_PERIOD_US / 100),
            ))
        }
        "TasksMax" => Some(("pids.max", value.to_string())),
        _ => None,
    }
}

/// Makes the job cgroup `name` under `base` with the limits of `limits` (the properties a
/// user scope would get, as interface files); returns its directory and the properties whose
/// file was written.
pub(crate) fn job_cgroup(
    base: &Path,
    controllers: &[String],
    name: &str,
    limits: &ResourceLimits,
) -> Option<(PathBuf, Vec<String>)> {
    let dir = base.join(name);
    std::fs::create_dir(&dir).ok()?;
    match fill(&dir, crate::limits::scope_properties(limits, controllers)) {
        Some(written) => Some((dir, written)),
        None => {
            let _ = remove_cgroup(&dir);
            None
        }
    }
}

/// Writes the interface files of `props` into the job cgroup `dir`; returns the properties
/// written, or `None` when a memory limit (`memory.high`, `memory.max`) could not be written:
/// the job must not start with a ceiling it was promised and does not have.
pub(crate) fn fill(dir: &Path, props: Vec<String>) -> Option<Vec<String>> {
    let mut written = Vec::new();
    for prop in props {
        let Some((file, value)) = cgroup_file(&prop) else {
            continue;
        };
        if std::fs::write(dir.join(file), value).is_ok() {
            written.push(prop);
        } else if matches!(file, "memory.high" | "memory.max") {
            return None;
        }
    }
    Some(written)
}

fn decimal(mut n: u32, buf: &mut [u8; 10]) -> &[u8] {
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    &buf[i..]
}

/// Writes the calling process's pid to the `cgroup.procs` file `path`; only async-signal-safe
/// calls, for the child between fork and exec.
fn join(path: &CStr) -> io::Result<()> {
    let mut buf = [0u8; 10];
    let text = decimal(unsafe { libc::getpid() } as u32, &mut buf);
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let n = unsafe { libc::write(fd, text.as_ptr().cast(), text.len()) };
    let err = io::Error::last_os_error();
    unsafe {
        libc::close(fd);
    }
    if n != text.len() as isize {
        return Err(err);
    }
    Ok(())
}

impl Delegated {
    fn job(&self, command: &str, limits: &ResourceLimits) -> Option<Wrapped> {
        retry_removal(&mut leftovers(), remove_cgroup);
        let (dir, props) = job_cgroup(&self.base, &self.controllers, &unit_name(), limits)?;
        let Ok(procs) = CString::new(dir.join("cgroup.procs").as_os_str().as_bytes()) else {
            let _ = std::fs::remove_dir(&dir);
            return None;
        };
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(command);
        unsafe {
            cmd.pre_exec(move || join(&procs));
        }
        let mut scope = Scope::bare(dir);
        scope.cpu_capped = props.iter().any(|p| p.starts_with("CPUQuota="));
        scope.hold = crate::limits::scope_hold(&props);
        scope.cleanup = Some(remove_cgroup);
        Some((cmd, Vec::new(), scope))
    }
}

fn unit_name() -> String {
    format!(
        "{UNIT_PREFIX}{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// The cgroup v2 a job tree runs in: a transient scope of the user manager, or a child cgroup
/// of the runner's delegated subtree. Everything started in it, detached descendants included,
/// stays in it and is killed with it (spec 3.11, 10). It is addressed by its directory, so it
/// stays reachable after the root process is gone.
pub(crate) struct Scope {
    dir: PathBuf,
    cpu_capped: bool,
    hold: MemoryHold,
    evidence: Arc<Mutex<MemoryEvidence>>,
    stop: Arc<AtomicBool>,
    cleanup: Option<Cleanup>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(cleanup) = self.cleanup {
            if self.dir.is_dir() && cleanup(&self.dir).is_err() {
                leftovers().push(self.dir.clone());
                let dir = self.dir.clone();
                std::thread::spawn(move || remove_when_unpopulated(&dir, cleanup));
            }
        }
    }
}

static LEFTOVERS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn leftovers() -> std::sync::MutexGuard<'static, Vec<PathBuf>> {
    LEFTOVERS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Removes the job cgroups of `list` that are gone or can be removed now, and keeps the
/// others for the next try.
pub(crate) fn retry_removal(list: &mut Vec<PathBuf>, cleanup: Cleanup) {
    list.retain(|dir| dir.is_dir() && cleanup(dir).is_err());
}

/// The `(oom_kill, max, high)` counters of a `memory.events` text.
pub(crate) fn event_counters(text: &str) -> (u64, u64, u64) {
    let get = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(' '))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    };
    (get("oom_kill"), get("max"), get("high"))
}

fn remember(seen: &Mutex<MemoryEvidence>, dir: &Path, events: Option<&str>) {
    let mut seen = seen.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(text) = events {
        let (kills, max, high) = event_counters(text);
        seen.oom_kills = seen.oom_kills.max(kills);
        seen.max_events = seen.max_events.max(max);
        seen.high_events = seen.high_events.max(high);
    }
    if let Ok(text) = std::fs::read_to_string(dir.join("memory.peak")) {
        if let Ok(bytes) = text.trim().parse::<u64>() {
            seen.peak_mb = seen.peak_mb.max(crate::limits::mb_of(bytes));
        }
    }
}

const FOLLOW_TICK: Duration = Duration::from_millis(5);
const FOLLOW_POLL_MS: libc::c_int = 100;
const FOLLOW_TRIES: u32 = 2000;

fn wait_change(file: &std::fs::File) {
    use std::os::fd::AsRawFd;
    let mut fds = [libc::pollfd {
        fd: file.as_raw_fd(),
        events: libc::POLLPRI,
        revents: 0,
    }];
    unsafe {
        libc::poll(fds.as_mut_ptr(), 1, FOLLOW_POLL_MS);
    }
}

/// Follows `memory.events` through the kernel's change notification (the cgroup is removed
/// within milliseconds of its last process dying, far sooner than a polling tick would see
/// the OOM kill) until the scope is gone or `stop` is set.
fn follow(dir: PathBuf, seen: Arc<Mutex<MemoryEvidence>>, stop: Arc<AtomicBool>) {
    use std::io::Seek;
    let path = dir.join("memory.events");
    let mut tries = 0;
    let mut file = loop {
        if stop.load(Ordering::Relaxed) || tries >= FOLLOW_TRIES {
            return;
        }
        match std::fs::File::open(&path) {
            Ok(file) => break file,
            Err(_) => {
                tries += 1;
                std::thread::sleep(FOLLOW_TICK);
            }
        }
    };
    loop {
        let mut text = String::new();
        if file.rewind().is_err() || file.read_to_string(&mut text).is_err() {
            return;
        }
        remember(&seen, &dir, Some(&text));
        if stop.load(Ordering::Relaxed) {
            return;
        }
        wait_change(&file);
    }
}

/// Removes the job cgroup `dir` once `cgroup.events` says no process is left in it, woken by
/// the kernel's change notification; gives up after a bounded number of wake-ups.
fn remove_when_unpopulated(dir: &Path, cleanup: Cleanup) {
    use std::io::Seek;
    let Ok(mut file) = std::fs::File::open(dir.join("cgroup.events")) else {
        let _ = cleanup(dir);
        return;
    };
    for _ in 0..FOLLOW_TRIES {
        let mut text = String::new();
        if file.rewind().is_err() || file.read_to_string(&mut text).is_err() {
            break;
        }
        if text.lines().any(|l| l.trim() == "populated 0") && cleanup(dir).is_ok() {
            return;
        }
        wait_change(&file);
    }
    let _ = cleanup(dir);
}

impl Scope {
    /// `sh -c command` inside a new job cgroup, or `None` when there is no cgroup mechanism
    /// here or the job sets the bus variables a user scope needs. For a user scope those
    /// variables are added for `systemd-run` and removed again before the shell; in a delegated
    /// subtree the shell joins its cgroup between fork and exec.
    pub(crate) fn wrap(
        command: &str,
        job_vars: &[&str],
        limits: &ResourceLimits,
    ) -> Option<Wrapped> {
        match backend_for_job()? {
            Backend::UserManager(systemd) if !job_owns_bus(job_vars) => {
                Some(user_scope(systemd, command, limits))
            }
            Backend::UserManager(_) => None,
            Backend::Delegated(delegated) => delegated.job(command, limits),
        }
    }

    /// The job cgroup a process started by an earlier runner lives in, when it is one of ours.
    pub(crate) fn of_pid(pid: u32) -> Option<Scope> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
        let dir = cgroup_dir(&text)?;
        let name = dir.file_name()?.to_str()?;
        if let Some(unit) = name.strip_suffix(".scope") {
            if !unit.starts_with(UNIT_PREFIX) {
                return None;
            }
            return Some(Scope::bare(dir));
        }
        let Some(Some(Backend::Delegated(d))) = BACKEND.get() else {
            return None;
        };
        if owner_pid(name).is_none() || dir.parent() != Some(d.base.as_path()) {
            return None;
        }
        let mut scope = Scope::bare(dir);
        scope.cleanup = Some(remove_cgroup);
        Some(scope)
    }

    fn bare(dir: PathBuf) -> Scope {
        Scope {
            dir,
            cpu_capped: false,
            hold: MemoryHold::Unheld,
            evidence: Arc::default(),
            stop: Arc::default(),
            cleanup: None,
        }
    }

    /// Starts following the scope's `memory.events` and `memory.peak` in the background, and
    /// keeps the highest values seen: the kernel's OOM kill can only be proven from what was
    /// read before the scope vanished.
    pub(crate) fn follow_memory(&self) {
        let (dir, seen, stop) = (self.dir.clone(), self.evidence.clone(), self.stop.clone());
        std::thread::spawn(move || follow(dir, seen, stop));
    }

    /// Reads `memory.events` and `memory.peak` once more, when the scope still exists.
    pub(crate) fn refresh_memory(&self) {
        let events = std::fs::read_to_string(self.dir.join("memory.events")).ok();
        remember(&self.evidence, &self.dir, events.as_deref());
    }

    /// What `refresh_memory` has remembered so far.
    pub(crate) fn memory_evidence(&self) -> MemoryEvidence {
        *self.evidence.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The pids of the scope's processes, when they can be read.
    pub(crate) fn pids(&self) -> Option<Vec<u32>> {
        procs(&self.dir).ok()
    }

    /// Whether a CPU quota was requested for the scope.
    pub(crate) fn cpu_capped(&self) -> bool {
        self.cpu_capped
    }

    pub(crate) fn memory_hold(&self) -> MemoryHold {
        self.hold
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
    /// none); `None` when its processes cannot be read. An empty job cgroup of a delegated
    /// subtree is removed, after its memory evidence is read once more.
    pub(crate) fn is_empty(&self) -> Option<bool> {
        match procs(&self.dir) {
            Ok(p) if p.is_empty() => {
                if let Some(cleanup) = self.cleanup {
                    self.refresh_memory();
                    let _ = cleanup(&self.dir);
                }
                Some(true)
            }
            Ok(_) => Some(false),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Some(true),
            Err(_) => None,
        }
    }
}

fn user_scope(systemd: &Systemd, command: &str, limits: &ResourceLimits) -> Wrapped {
    let unit = unit_name();
    let mut cmd = Command::new(&systemd.exe);
    cmd.args(["--user", "--scope", "--quiet", "--collect"])
        .arg(format!("--unit={unit}"));
    let props = crate::limits::scope_properties(limits, &systemd.controllers);
    for prop in &props {
        cmd.arg("-p").arg(prop);
    }
    cmd.args(["--", "/usr/bin/env"]);
    let mut extra = Vec::new();
    for var in BUS_VARS {
        if let Some(value) = std::env::var_os(var) {
            cmd.args(["-u", var]);
            extra.push((OsString::from(var), value));
        }
    }
    cmd.args(["/bin/sh", "-c", command]);
    let dir = systemd.slice.join(format!("{unit}.scope"));
    let mut scope = Scope::bare(dir);
    scope.cpu_capped = props.iter().any(|p| p.starts_with("CPUQuota="));
    scope.hold = crate::limits::scope_hold(&props);
    (cmd, extra, scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lr_abstractions::Enforce;

    struct Fake(PathBuf);

    impl Fake {
        fn new(tag: &str) -> Fake {
            let root = std::env::temp_dir().join(format!(
                "lr-cgroup-{tag}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Fake(root)
        }

        fn group(&self, rel: &str, controllers: &str, procs: &str) -> PathBuf {
            let dir = self.0.join(rel);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("cgroup.controllers"), controllers).unwrap();
            std::fs::write(dir.join("cgroup.procs"), procs).unwrap();
            std::fs::write(dir.join("cgroup.kill"), "").unwrap();
            std::fs::write(dir.join("cgroup.subtree_control"), "").unwrap();
            dir
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const SERVICE: &str = "0::/system.slice/al-localrunner-runner-box.service\n";

    fn limit(mem_mb: u64) -> ResourceLimits {
        ResourceLimits {
            mode: Enforce::Limit,
            mem_mb: Some(mem_mb),
            ..ResourceLimits::default()
        }
    }

    fn remove_all(dir: &Path) -> io::Result<()> {
        std::fs::remove_dir_all(dir)
    }

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
    fn the_runner_pid_is_read_from_our_cgroup_names_only() {
        assert_eq!(owner_pid("lr-4242-7.scope"), Some(4242));
        assert_eq!(owner_pid("lr-4242-7"), Some(4242));
        assert_eq!(owner_pid("lr-4242.scope"), None);
        assert_eq!(owner_pid("lr-4242-x.scope"), None);
        assert_eq!(owner_pid("app-4242-7.scope"), None);
        assert_eq!(owner_pid("lr-4242-7.service"), None);
        assert_eq!(owner_pid("runner"), None);
    }

    #[test]
    fn the_counters_are_read_from_memory_events() {
        let text = "low 0\nhigh 5\nmax 3\noom 1\noom_kill 2\noom_group_kill 0\n";
        assert_eq!(event_counters(text), (2, 3, 5));
        assert_eq!(event_counters("max 3\n"), (0, 3, 0));
        assert_eq!(event_counters("oom_kill x\nmaxx 4\nhighx 1\n"), (0, 0, 0));
    }

    #[test]
    fn a_gone_scope_keeps_what_was_remembered() {
        let scope = Scope::bare(PathBuf::from("/sys/fs/cgroup/no-such-lr-scope.scope"));
        scope.evidence.lock().unwrap().oom_kills = 1;
        scope.refresh_memory();
        assert_eq!(scope.memory_evidence().oom_kills, 1);
    }

    #[test]
    fn a_job_setting_bus_variables_owns_them() {
        assert!(job_owns_bus(&["PATH", "xdg_runtime_dir"]));
        assert!(job_owns_bus(&["DBUS_SESSION_BUS_ADDRESS"]));
        assert!(!job_owns_bus(&["PATH", "HOME"]));
    }

    #[test]
    fn a_scope_that_never_came_up_is_empty() {
        let scope = Scope::bare(PathBuf::from("/sys/fs/cgroup/no-such-lr-scope.scope"));
        assert_eq!(scope.is_empty(), Some(true));
    }

    #[test]
    fn the_runner_moves_into_its_leaf_before_enabling_memory_for_children() {
        let base = Path::new("/sys/fs/cgroup/system.slice/x.service");
        assert_eq!(
            setup_steps(base, 42),
            vec![
                Step::Mkdir(base.join("runner")),
                Step::Write(base.join("runner/cgroup.procs"), "42".to_string()),
                Step::Write(base.join("cgroup.subtree_control"), "+memory".to_string()),
            ]
        );
    }

    #[test]
    fn a_service_owning_its_cgroup_gets_a_delegated_subtree() {
        let fake = Fake::new("own");
        let base = fake.group(
            "system.slice/al-localrunner-runner-box.service",
            "cpu io memory pids\n",
            "42\n",
        );
        let d = delegate(&fake.0, SERVICE, 42).unwrap();
        assert_eq!(d.base, base);
        assert_eq!(d.controllers, ["memory", "cpu", "pids"]);
        assert_eq!(
            std::fs::read_to_string(base.join("runner/cgroup.procs")).unwrap(),
            "42"
        );
    }

    #[test]
    fn a_runner_already_in_its_leaf_keeps_the_subtree() {
        let fake = Fake::new("leaf");
        let base = fake.group("system.slice/x.service", "memory pids\n", "");
        std::fs::write(base.join("cgroup.subtree_control"), "memory pids\n").unwrap();
        fake.group("system.slice/x.service/runner", "memory pids\n", "42\n");
        let d = delegate(&fake.0, "0::/system.slice/x.service/runner\n", 42).unwrap();
        assert_eq!(d.base, base);
        assert_eq!(d.controllers, ["memory", "pids"]);
        assert!(!base.join("runner/runner").exists());
    }

    #[test]
    fn no_subtree_without_memory_other_processes_or_below_the_root() {
        let fake = Fake::new("none");
        fake.group(
            "system.slice/al-localrunner-runner-box.service",
            "cpu pids\n",
            "42\n",
        );
        assert_eq!(delegate(&fake.0, SERVICE, 42), None);
        let fake = Fake::new("others");
        fake.group(
            "system.slice/al-localrunner-runner-box.service",
            "memory\n",
            "7\n42\n",
        );
        assert_eq!(delegate(&fake.0, SERVICE, 42), None);
        let fake = Fake::new("root");
        std::fs::write(fake.0.join("cgroup.controllers"), "memory\n").unwrap();
        std::fs::write(fake.0.join("cgroup.procs"), "42\n").unwrap();
        assert_eq!(delegate(&fake.0, "0::/\n", 42), None);
        assert!(!fake.0.join("runner").exists());
    }

    #[test]
    fn no_subtree_on_a_kernel_without_cgroup_kill() {
        let fake = Fake::new("nokill");
        let base = fake.group(
            "system.slice/al-localrunner-runner-box.service",
            "memory\n",
            "42\n",
        );
        std::fs::remove_file(base.join("cgroup.kill")).unwrap();
        assert_eq!(delegate(&fake.0, SERVICE, 42), None);
        assert!(!base.join("runner").exists());
    }

    #[test]
    fn a_job_cgroup_without_its_memory_ceiling_is_refused() {
        let fake = Fake::new("fill");
        let props = scope_properties_of(&limit(1000));
        assert!(fill(&fake.0, props.clone()).is_some());
        let dir = fake.0.join("lr-1-9");
        std::fs::create_dir_all(dir.join("memory.high")).unwrap();
        assert_eq!(fill(&dir, props), None);
        let dir = fake.0.join("lr-1-10");
        std::fs::create_dir_all(dir.join("cpu.max")).unwrap();
        let capped = ResourceLimits {
            cpu_millis: Some(1000),
            ..limit(1000)
        };
        let written = fill(&dir, scope_properties_of(&capped)).unwrap();
        assert!(!written.iter().any(|p| p.starts_with("CPUQuota=")));
        assert_eq!(crate::limits::scope_hold(&written), MemoryHold::Scope);
    }

    fn scope_properties_of(limits: &ResourceLimits) -> Vec<String> {
        crate::limits::scope_properties(limits, &["memory".to_string(), "cpu".to_string()])
    }

    #[test]
    fn leftover_job_cgroups_are_retried_until_removed() {
        let fake = Fake::new("leftover");
        let busy = fake.group("lr-1-11", "memory\n", "9\n");
        let gone = fake.0.join("lr-1-12");
        let mut list = vec![busy.clone(), gone];
        fn refuse(_: &Path) -> io::Result<()> {
            Err(io::Error::other("busy"))
        }
        retry_removal(&mut list, refuse);
        assert_eq!(list, vec![busy.clone()]);
        retry_removal(&mut list, remove_all);
        assert!(list.is_empty());
        assert!(!busy.exists());
    }

    #[test]
    fn no_subtree_when_controllers_cannot_be_enabled() {
        let fake = Fake::new("locked");
        let base = fake.group(
            "system.slice/al-localrunner-runner-box.service",
            "memory\n",
            "42\n",
        );
        std::fs::remove_file(base.join("cgroup.subtree_control")).unwrap();
        std::fs::create_dir(base.join("cgroup.subtree_control")).unwrap();
        assert_eq!(delegate(&fake.0, SERVICE, 42), None);
        assert_eq!(
            std::fs::read_to_string(base.join("cgroup.procs")).unwrap(),
            "42"
        );
    }

    #[test]
    fn no_subtree_when_the_cgroup_is_not_writable() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let fake = Fake::new("readonly");
        let base = fake.group(
            "system.slice/al-localrunner-runner-box.service",
            "memory\n",
            "42\n",
        );
        let read_only = std::fs::Permissions::from_mode(0o555);
        std::fs::set_permissions(&base, read_only).unwrap();
        let found = delegate(&fake.0, SERVICE, 42);
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(found, None);
        assert!(!base.join("runner").exists());
    }

    #[test]
    fn a_job_cgroup_holds_memory_at_the_ceiling_with_the_backstop_above() {
        let fake = Fake::new("job");
        let controllers = vec!["memory".to_string(), "cpu".to_string(), "pids".to_string()];
        let limits = ResourceLimits {
            cpu_millis: Some(2000),
            procs: Some(512),
            ..limit(1000)
        };
        let (dir, props) = job_cgroup(&fake.0, &controllers, "lr-1-1", &limits).unwrap();
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
        assert_eq!(read("memory.high"), (1000u64 * 1024 * 1024).to_string());
        assert_eq!(read("memory.max"), (1100u64 * 1024 * 1024).to_string());
        assert_eq!(read("cpu.max"), "200000 100000");
        assert_eq!(read("pids.max"), "512");
        assert_eq!(crate::limits::scope_hold(&props), MemoryHold::Scope);
        assert!(!props.iter().any(|p| p.starts_with("OOMPolicy=")));
    }

    #[test]
    fn a_job_cgroup_without_the_memory_controller_holds_nothing() {
        let fake = Fake::new("nomem");
        let (dir, props) = job_cgroup(&fake.0, &[], "lr-1-2", &limit(1000)).unwrap();
        assert!(!dir.join("memory.high").exists());
        assert_eq!(crate::limits::scope_hold(&props), MemoryHold::Unheld);
        assert_eq!(job_cgroup(&fake.0, &[], "lr-1-2", &limit(1000)), None);
    }

    #[test]
    fn the_properties_map_to_interface_files() {
        assert_eq!(
            cgroup_file("MemorySwapMax=0"),
            Some(("memory.swap.max", "0".to_string()))
        );
        assert_eq!(
            cgroup_file("CPUQuota=50%"),
            Some(("cpu.max", "50000 100000".to_string()))
        );
        assert_eq!(cgroup_file("OOMPolicy=continue"), None);
        assert_eq!(cgroup_file("CPUQuota=x%"), None);
    }

    #[test]
    fn the_job_cgroup_events_are_remembered() {
        let fake = Fake::new("events");
        let dir = fake.group("lr-1-3", "memory\n", "9\n");
        std::fs::write(
            dir.join("memory.events"),
            "low 0\nhigh 4\nmax 1\noom 1\noom_kill 1\n",
        )
        .unwrap();
        std::fs::write(dir.join("memory.peak"), (3u64 * 1024 * 1024).to_string()).unwrap();
        let scope = Scope::bare(dir);
        scope.refresh_memory();
        let seen = scope.memory_evidence();
        assert_eq!(
            (seen.high_events, seen.max_events, seen.oom_kills),
            (4, 1, 1)
        );
        assert_eq!(seen.peak_mb, 3);
        assert_eq!(scope.pids(), Some(vec![9]));
    }

    #[test]
    fn an_empty_job_cgroup_is_removed_and_a_busy_one_kept() {
        let fake = Fake::new("cleanup");
        let busy = fake.group("lr-1-4", "memory\n", "9\n");
        fake.group("lr-1-4/runner", "memory\n", "10\n");
        let mut scope = Scope::bare(busy.clone());
        scope.cleanup = Some(remove_all);
        assert_eq!(scope.pids(), Some(vec![9, 10]));
        assert_eq!(scope.is_empty(), Some(false));
        assert!(busy.is_dir());
        let empty = fake.group("lr-1-5", "memory\n", "");
        std::fs::write(empty.join("memory.events"), "high 2\n").unwrap();
        let mut scope = Scope::bare(empty.clone());
        scope.cleanup = Some(remove_all);
        assert_eq!(scope.is_empty(), Some(true));
        assert!(!empty.exists());
        assert_eq!(scope.memory_evidence().high_events, 2);
    }

    #[test]
    fn the_child_hook_writes_its_own_pid() {
        let fake = Fake::new("join");
        let file = fake.0.join("cgroup.procs");
        std::fs::write(&file, "").unwrap();
        join(&CString::new(file.as_os_str().as_bytes()).unwrap()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            std::process::id().to_string()
        );
        assert!(
            join(&CString::new(fake.0.join("missing/x").as_os_str().as_bytes()).unwrap()).is_err()
        );
        let mut buf = [0u8; 10];
        assert_eq!(decimal(0, &mut buf), b"0");
        assert_eq!(decimal(u32::MAX, &mut buf), b"4294967295");
    }
}
