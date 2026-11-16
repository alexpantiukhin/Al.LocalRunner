use crate::common::TempDir;
use crate::kit::{alive, Gate, Mode, Release};
use crate::support::*;
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_core::JobState;
use std::path::Path;

fn file_exists_check(path: &Path) -> String {
    if cfg!(windows) {
        format!("if exist \"{}\" (exit 0) else (exit 1)", path.display())
    } else {
        format!("test -f '{}'", path.display())
    }
}

fn service_toml(dir: &TempDir, gate: &Gate) -> String {
    let ready = dir.path().join("svc.ready");
    format!(
        "{}\n[service.svc]\nstart = {}\nready_check = {}\nstop = \"exit 0\"\n\
         ready_timeout_s = 60\nstop_timeout_s = 1\nmemory_mb = 64\nlifetime = \"per_job\"\n\
         max_lifetime_s = 600\n",
        runner_toml(),
        toml_str(&gate.command_with(Mode::Tree, "svc", Some(&ready))),
        toml_str(&file_exists_check(&ready)),
    )
}

// scenario: 16.15
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_per_job_service_tree_is_ready_for_the_job_and_gone_after_it() {
    let dir = TempDir::new("e2e-service");
    let gate = Gate::new().await;
    let step_gate = Gate::new().await;
    let release = Release::new(&[&gate, &step_gate]);
    let layout = layout(&dir, "machine", "");
    let runner_dir = layout.runner_dir("r1");
    std::fs::create_dir_all(&runner_dir).unwrap();
    std::fs::write(runner_dir.join("runner.toml"), service_toml(&dir, &gate)).unwrap();
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runner = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    runner_online(&layout, "r1").await;

    let profile = format!(
        "[profile.with-service]
command = {}
timeout_s = 120
mem_mb = 64
services = [\"svc\"]
",
        toml_str(&step_gate.command(Mode::Wait, "step"))
    );
    let repo = repo(&dir, &profile);
    let (client, job) = submit(&layout, &repo, "with-service", None).await;
    let tree = [
        gate.started("svc", "child").await,
        gate.started("svc", "grandchild").await,
    ];
    let processes = [tree[0].process(), tree[1].process()];
    step_gate.started("step", "child").await;
    assert!(
        processes.iter().all(|&p| alive(p)),
        "the service is alive while the step runs"
    );
    step_gate.open();

    let ended = until_exit(client, &job, HANG).await;
    assert_eq!(ended.state, JobState::Passed, "{ended:?}");
    for p in &tree {
        p.gone("the service tree to be gone after the job").await;
    }

    runner.stop();
    hub_role.stop();
    broker.stop();
    drop(release);
    remove(dir).await;
}
