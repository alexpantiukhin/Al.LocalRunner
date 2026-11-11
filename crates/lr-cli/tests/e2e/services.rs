use crate::common::TempDir;
use crate::helper::{helper_command, Mode};
use crate::support::*;
use lr_abstractions::{Liveness, ProcessInspector};
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_core::JobState;
use lr_exec::{process_start, OsProcessInspector};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn alive(pid: u32, start: u64) -> bool {
    OsProcessInspector.liveness(pid, start) == Liveness::Alive
}

/// Opens the helpers' gates when the test ends, even by a panic, and waits until the watched
/// processes are gone, so none holds the temporary directory.
struct Release {
    gates: Vec<PathBuf>,
    watched: Vec<(u32, u64)>,
}

impl Release {
    fn watch(&mut self, pid: u32) {
        if let Some(start) = process_start(pid) {
            self.watched.push((pid, start));
        }
    }
}

impl Drop for Release {
    fn drop(&mut self) {
        for gate in &self.gates {
            let _ = std::fs::write(gate, b"open");
        }
        let started = Instant::now();
        while self.watched.iter().any(|&(p, s)| alive(p, s))
            && started.elapsed() < Duration::from_secs(20)
        {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn file_exists_check(path: &Path) -> String {
    if cfg!(windows) {
        format!("if exist \"{}\" (exit 0) else (exit 1)", path.display())
    } else {
        format!("test -f '{}'", path.display())
    }
}

fn service_toml(dir: &TempDir) -> String {
    let pids = dir.path().join("svc.pid");
    let gate = dir.path().join("gate");
    let child = dir.path().join("svc.pid.child");
    format!(
        "{}\n[service.svc]\nstart = {}\nready_check = {}\nstop = \"exit 0\"\n\
         ready_timeout_s = 60\nstop_timeout_s = 1\nmemory_mb = 64\nlifetime = \"per_job\"\n\
         max_lifetime_s = 600\n",
        runner_toml(),
        toml_str(&helper_command(Mode::Tree, &pids, &gate)),
        toml_str(&file_exists_check(&child)),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_per_job_service_tree_is_ready_for_the_job_and_gone_after_it() {
    let dir = TempDir::new("e2e-service");
    let step_gate = dir.path().join("step-gate");
    let mut release = Release {
        gates: vec![dir.path().join("gate"), step_gate.clone()],
        watched: Vec::new(),
    };
    let layout = layout(&dir, "machine", "");
    let runner_dir = layout.runner_dir("r1");
    std::fs::create_dir_all(&runner_dir).unwrap();
    std::fs::write(runner_dir.join("runner.toml"), service_toml(&dir)).unwrap();
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runner = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    runner_online(&layout, "r1").await;

    let step_pids = dir.path().join("step.pid");
    let profile = format!(
        "[profile.with-service]
command = {}
timeout_s = 120
mem_mb = 64
services = [\"svc\"]
",
        toml_str(&helper_command(Mode::Wait, &step_pids, &step_gate))
    );
    let repo = repo(&dir, &profile);
    let (client, job) = submit(&layout, &repo, "with-service", None).await;
    let pids = dir.path().join("svc.pid");
    let child = helper_pid(&pids, "child").await;
    let grandchild = helper_pid(&pids, "grandchild").await;
    let tree: Vec<(u32, u64)> = [child, grandchild]
        .into_iter()
        .filter_map(|pid| process_start(pid).map(|s| (pid, s)))
        .collect();
    release.watched.extend(tree.iter().copied());
    let step = helper_pid(&step_pids, "child").await;
    release.watch(step);
    assert_eq!(tree.len(), 2, "the service tree runs");
    assert!(
        tree.iter().all(|&(p, s)| alive(p, s)),
        "the service is alive while the step runs"
    );
    std::fs::write(&step_gate, b"open").unwrap();

    let ended = until_exit(client, &job, Duration::from_secs(90)).await;
    assert_eq!(ended.state, JobState::Passed, "{ended:?}");
    eventually(
        "the service tree to be gone after the job",
        Duration::from_secs(15),
        || async { tree.iter().all(|&(p, s)| !alive(p, s)).then_some(()) },
    )
    .await;

    runner.stop();
    hub_role.stop();
    broker.stop();
    drop(release);
    remove(dir).await;
}
