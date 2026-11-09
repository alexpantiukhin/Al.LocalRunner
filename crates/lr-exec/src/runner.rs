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
/// macOS), so children that leave the group are killed while they were seen; a child that
/// detaches and loses its parent before it is seen escapes (best effort; cgroup v2 is not
/// used yet).
///
/// Stops: a step timeout kills the tree at once without a soft signal; [`lr_abstractions::Stop::Cancel`]
/// sends Ctrl+Break to the process group on Windows or SIGTERM to the group and the tracked
/// descendants elsewhere, then kills the tree after `cancel_grace` on the runner clock. Ctrl+Break
/// reaches the step only through the console the runner shares with it, so the daemon is
/// started with a (hidden) console; a runner without one gives steps a hidden console of their
/// own and a cancel then ends with the kill. A step ends only when its tree is empty; leftover
/// descendants of a step that exited by itself are killed too. Output after a stop or timeout is
/// passed on for at most one second, so a blocked consumer cannot delay the result. Dropping
/// a running `run_job` future kills its tree.
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
        self.services
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(process)
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
            let _ = service.child.wait().await;
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
