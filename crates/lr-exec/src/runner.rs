use crate::probe::{probe, Probe};
use crate::spawn::{spawn_service, Launch, Tree};
use async_trait::async_trait;
use lr_abstractions::{
    Clock, JobOutcome, JobOutput, JobSpec, ProcessError, ProcessRef, ProcessRunner, SpawnSpec,
    StopSignal,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Child;
use tokio::time::Instant;

const POLL: Duration = Duration::from_millis(20);
const KILL_LIMIT: Duration = Duration::from_secs(10);

struct Service {
    child: Child,
    tree: Tree,
}

/// The real [`ProcessRunner`]: runs job steps and runner services as OS processes, each in
/// its own process container, and kills whole trees (spec 6, 7.3, 3.11).
///
/// Every command runs through the platform shell (`%ComSpec% /D /S /C` on Windows,
/// `/bin/sh -c` elsewhere) in the job directory with exactly the environment of the spec plus
/// the secrets of that step; nothing of the runner environment is inherited (spec 3.13).
///
/// Containers: Windows puts the process, created suspended, into a Job Object that kills
/// every descendant, detached ones included (full guarantee). Unix starts a new process group
/// and also tracks descendants by walking parent links (`/proc` on Linux, `proc_pidinfo` on
/// macOS), so children that leave the group are killed while they were seen (best effort).
///
/// Known limitation: Linux cgroup v2 (`systemd-run --user --scope`, the full guarantee of
/// spec 6 and 10) is not used yet, so on Linux a child that detaches (`setsid`, double fork)
/// and loses its parent between two scans (100 ms) escapes the kill.
///
/// Stops: a step timeout kills the tree at once without a soft signal; a
/// [`lr_abstractions::Stop::Cancel`] sends Ctrl+Break to the process group on Windows or
/// SIGTERM to the group and the tracked descendants elsewhere, then kills the tree after
/// `cancel_grace` on the runner clock. A cancelled step's output ends with a [`crate::cancel_line`]
/// carrying the reason of the stop request.
///
/// Ctrl+Break reaches a step only through the console the runner shares with it, so the
/// process hosting the runner (the daemon, the Windows service host) must allocate a hidden
/// console. Without one, steps get a hidden console of their own, the soft signal cannot be
/// delivered, and the step output says so with a line starting with
/// [`crate::SOFT_STOP_NOT_DELIVERED`]; the tree is then killed after the grace.
///
/// A step ends only when its tree is empty, or 10 s after the kill when a process refuses to
/// die (for example in uninterruptible sleep); leftover descendants of a step that exited by
/// itself are killed too. Output after a stop or timeout is passed on for at most one second,
/// so a blocked consumer cannot delay the result. Dropping a running `run_job` future kills
/// its tree. Services that exited by themselves are dropped from the registry.
pub struct ExecRunner {
    clock: Arc<dyn Clock>,
    services: Mutex<HashMap<ProcessRef, Service>>,
}

impl ExecRunner {
    /// A runner timing steps, grace and timestamps on `clock`.
    pub fn new(clock: Arc<dyn Clock>) -> ExecRunner {
        ExecRunner {
            clock,
            services: Mutex::new(HashMap::new()),
        }
    }

    fn take_service(&self, process: &ProcessRef) -> Option<Service> {
        self.sweep();
        self.services
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(process)
    }

    fn sweep(&self) {
        self.services
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, s| !(matches!(s.child.try_wait(), Ok(Some(_))) && s.tree.is_empty()));
    }

    /// Number of services started by [`ProcessRunner::spawn`] that still run; services whose
    /// whole tree has exited are dropped from the registry first.
    pub fn service_count(&self) -> usize {
        self.sweep();
        self.services
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }
}

async fn wait_empty(tree: &Tree, mut child: Option<&mut Child>) {
    let until = Instant::now() + KILL_LIMIT;
    tree.kill();
    loop {
        if let Some(child) = child.as_mut() {
            let _ = child.try_wait();
        }
        if tree.is_empty() || Instant::now() >= until {
            break;
        }
        tokio::time::sleep(POLL).await;
        tree.kill();
    }
}

#[cfg(windows)]
async fn kill_unmanaged(pid: u32, start: u64) {
    use crate::os_windows::terminate;
    use crate::probe::descendants;
    let until = Instant::now() + KILL_LIMIT;
    let mut tree = descendants(&[(pid, start)]);
    terminate(pid, start);
    while !tree.is_empty() && Instant::now() < until {
        for (child, child_start) in &tree {
            terminate(*child, *child_start);
        }
        let mut next = descendants(&tree);
        next.extend(
            tree.iter()
                .filter(|(p, s)| probe(*p) == Probe::Alive(*s))
                .copied(),
        );
        tree = next;
        tokio::time::sleep(POLL).await;
    }
}

#[cfg(unix)]
async fn kill_unmanaged(pid: u32, start: u64) {
    wait_empty(&Tree::adopt(pid, start), None).await;
}

#[async_trait]
impl ProcessRunner for ExecRunner {
    async fn run_job(&self, spec: JobSpec, output: &dyn JobOutput, stop: StopSignal) -> JobOutcome {
        crate::job::run_job(&spec, self.clock.as_ref(), output, stop).await
    }

    async fn spawn(&self, spec: SpawnSpec) -> Result<ProcessRef, ProcessError> {
        let launch = Launch {
            command: &spec.command,
            cwd: &spec.cwd,
            env: &spec.env,
            secrets: &spec.secrets,
        };
        let (mut child, tree) =
            spawn_service(&launch).map_err(|e| ProcessError::Spawn(e.to_string()))?;
        let pid = child
            .id()
            .ok_or_else(|| ProcessError::Spawn("process exited at once".to_string()))?;
        let process = ProcessRef {
            pid,
            start: tree.start(),
        };
        if process.start.is_none() {
            wait_empty(&tree, Some(&mut child)).await;
            return Err(ProcessError::Unverifiable(pid));
        }
        self.sweep();
        self.services
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(process.clone(), Service { child, tree });
        Ok(process)
    }

    async fn kill_tree(&self, process: &ProcessRef) -> Result<(), ProcessError> {
        let start = process
            .start
            .ok_or(ProcessError::Unverifiable(process.pid))?;
        if let Some(mut service) = self.take_service(process) {
            wait_empty(&service.tree, Some(&mut service.child)).await;
            let _ = tokio::time::timeout(KILL_LIMIT, service.child.wait()).await;
            return Ok(());
        }
        match probe(process.pid) {
            Probe::Alive(s) if s == start => kill_unmanaged(process.pid, start).await,
            Probe::Unreadable => return Err(ProcessError::Unverifiable(process.pid)),
            _ => {}
        }
        Ok(())
    }
}
