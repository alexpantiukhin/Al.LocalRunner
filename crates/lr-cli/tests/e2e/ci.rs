use crate::common::TempDir;
use crate::helper::spawn_step;
use crate::kit::*;
use crate::support::*;
use lr_cli::paths::Layout;
use lr_core::{JobId, JobState};
use lr_exec::process_start;
use lr_proto::{QueueItem, ReasonCode, WireSource};
use std::path::Path;
use std::time::Duration;

const ENDS: Duration = Duration::from_secs(120);
const CI_FIRST: &str =
    "[[rule]]\nname = \"ci-first\"\nwhen = { client_label = \"ci\" }\npriority = 100\n";

fn github_env() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_REPOSITORY", "owner/project"),
        ("GITHUB_WORKFLOW", "checks"),
        ("GITHUB_JOB", "build"),
        ("GITHUB_RUN_ID", "4242"),
        ("GITHUB_RUN_ATTEMPT", "1"),
    ]
}

fn run_args(ci: &Layout) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--state-dir".into(),
        ci.root.display().to_string(),
        "--config-dir".into(),
        ci.config.display().to_string(),
    ];
    args.extend(
        ["run", "ci", "--as", "ci", "--platform", PLATFORM]
            .iter()
            .map(|s| s.to_string()),
    );
    args
}

async fn ci_order(main: &Layout) -> QueueItem {
    eventually("the ci order queued", ENDS, || async {
        status(main)
            .await?
            .queue
            .into_iter()
            .find(|q| q.summary.client_label == "ci")
    })
    .await
}

async fn running(main: &Layout, job: &JobId) {
    eventually(&format!("{job} running"), ENDS, || async {
        status(main)
            .await?
            .running
            .iter()
            .any(|r| &r.summary.job == job)
            .then_some(())
    })
    .await;
}

fn pid_of(pids: &Path, role: &str) -> Option<u32> {
    let mut name = pids.file_name().unwrap().to_os_string();
    name.push(format!(".{role}"));
    std::fs::read_to_string(pids.with_file_name(name))
        .ok()?
        .trim()
        .parse()
        .ok()
}

// scenario: 16.16
// scenario: 16.18
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn github_run_killed_by_signal_cancels_its_job() {
    let dir = TempDir::new("e2e-ci");
    let gate = dir.path().join("gate");
    let ci_gate = dir.path().join("ci-gate");
    let mut release = Release::new(&[&gate, &ci_gate]);
    let (main, roles) = machine(&dir, "main", CI_FIRST, &["r1"], &platform_runner_toml()).await;
    let (ci, ci_hub) = joined_hub(&dir, &main, "ci", "ci-box", Some("ci")).await;
    let profiles = format!(
        "{}\n{}\n{}",
        held_profile("first", dir.path(), &gate, 120, "mem_mb = 256\n"),
        echo_profile("ordinary", "ordinary-after-ci", 60, "mem_mb = 256\n"),
        held_profile(
            "ci",
            dir.path(),
            &ci_gate,
            120,
            &format!("mem_mb = 256\nplatforms = [\"{PLATFORM}\"]\n")
        ),
    );
    let repo = repo(&dir, &profiles);
    push_origin(&dir, &repo);

    let (first_client, first) = submit(&main, &repo, "first", None).await;
    release.watch(started(dir.path(), "first", "child").await);
    let (ordinary_client, ordinary) = submit(&main, &repo, "ordinary", None).await;
    queued(&main, &ordinary).await;

    let step_pids = dir.path().join("step.pid");
    let exe = Path::new(env!("CARGO_BIN_EXE_localrunner"));
    let mut step = spawn_step(&step_pids, exe, &run_args(&ci), &repo, &github_env());
    let run_pid = eventually("the run process", ENDS, || {
        let pid = pid_of(&step_pids, "run");
        async move { pid }
    })
    .await;
    let run_process = (run_pid, process_start(run_pid).expect("run runs"));
    release.watch(run_process);
    let order = ci_order(&main).await;
    assert_eq!(
        order.position, 1,
        "priority 100 puts the ci order ahead of the older ordinary one: {order:?}"
    );
    let ci_job = order.summary.job.clone();

    open(&gate);
    let a = until_exit(first_client, &first, ENDS).await;
    assert_eq!(a.state, JobState::Passed, "{a:?}");
    running(&main, &ci_job).await;
    let helper = started(dir.path(), "ci", "child").await;
    release.watch(helper);
    let waiting = queued(&main, &ordinary).await;
    assert_eq!(waiting.position, 1, "{waiting:?}");

    step.kill().unwrap();
    step.wait().unwrap();
    let cancelled = row(&main, &ci_job).await;
    assert_eq!(cancelled.state, JobState::Cancelled, "{cancelled:?}");
    assert_eq!(
        cancelled.reason,
        Some(ReasonCode::SenderGone),
        "{cancelled:?}"
    );
    assert_eq!(cancelled.client_label, "ci", "{cancelled:?}");
    assert_eq!(cancelled.source, Some(WireSource::Github), "{cancelled:?}");
    gone(
        "the ci job tree and the run process",
        &[helper, run_process],
    )
    .await;

    let b = until_exit(ordinary_client, &ordinary, ENDS).await;
    assert_eq!(b.state, JobState::Passed, "{b:?}");
    assert!(b.output.contains("ordinary-after-ci"), "{}", b.output);

    ci_hub.stop();
    roles.stop();
    drop(release);
    remove(dir).await;
}
