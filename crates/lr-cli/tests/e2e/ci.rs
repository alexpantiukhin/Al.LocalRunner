use crate::common::TempDir;
use crate::helper::Step;
use crate::kit::*;
use crate::support::*;
use lr_cli::paths::Layout;
use lr_core::{JobId, JobState};
use lr_proto::{QueueItem, ReasonCode, WireSource};
use std::path::Path;
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
    until_hub(main, "the ci order queued", || async {
        status(main)
            .await?
            .queue
            .into_iter()
            .find(|q| q.summary.client_label == "ci")
    })
    .await
}

async fn running(main: &Layout, job: &JobId) {
    until_hub(main, &format!("{job} running"), || async {
        status(main)
            .await?
            .running
            .iter()
            .any(|r| &r.summary.job == job)
            .then_some(())
    })
    .await;
}

// scenario: 16.16
// scenario: 16.18
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn github_run_killed_by_signal_cancels_its_job() {
    let dir = TempDir::new("e2e-ci");
    let gate = Gate::new().await;
    let ci_gate = Gate::new().await;
    let release = Release::new(&[&gate, &ci_gate]);
    let (main, roles) = machine(&dir, "main", CI_FIRST, &["r1"], &platform_runner_toml()).await;
    let (ci, ci_hub) = joined_hub(&dir, &main, "ci", "ci-box", Some("ci")).await;
    let profiles = format!(
        "{}\n{}\n{}",
        held_profile("first", &gate, 120, "mem_mb = 256\n"),
        echo_profile("ordinary", "ordinary-after-ci", 60, "mem_mb = 256\n"),
        held_profile(
            "ci",
            &ci_gate,
            120,
            &format!("mem_mb = 256\nplatforms = [\"{PLATFORM}\"]\n")
        ),
    );
    let repo = repo(&dir, &profiles);
    push_origin(&dir, &repo);

    let (first_client, first) = submit(&main, &repo, "first", None).await;
    gate.started("first", "child").await;
    let (ordinary_client, ordinary) = submit(&main, &repo, "ordinary", None).await;
    queued(&main, &ordinary).await;

    let exe = Path::new(env!("CARGO_BIN_EXE_localrunner"));
    let mut step = Step::spawn(exe, &run_args(&ci), &repo, &github_env());
    let order = ci_order(&main).await;
    assert_eq!(
        order.position, 1,
        "priority 100 puts the ci order ahead of the older ordinary one: {order:?}"
    );
    let ci_job = order.summary.job.clone();

    gate.open();
    let a = until_exit(first_client, &first, HANG).await;
    assert_eq!(a.state, JobState::Passed, "{a:?}");
    running(&main, &ci_job).await;
    let helper = ci_gate.started("ci", "child").await;
    let waiting = queued(&main, &ordinary).await;
    assert_eq!(waiting.position, 1, "{waiting:?}");

    step.child.kill().unwrap();
    step.child.wait().unwrap();
    let cancelled = row(&main, &ci_job).await;
    assert_eq!(cancelled.state, JobState::Cancelled, "{cancelled:?}");
    assert_eq!(
        cancelled.reason,
        Some(ReasonCode::SenderGone),
        "{cancelled:?}"
    );
    assert_eq!(cancelled.client_label, "ci", "{cancelled:?}");
    assert_eq!(cancelled.source, Some(WireSource::Github), "{cancelled:?}");
    helper.gone("the ci job tree").await;
    step.gone("the run process").await;

    let b = until_exit(ordinary_client, &ordinary, HANG).await;
    assert_eq!(b.state, JobState::Passed, "{b:?}");
    assert!(b.output.contains("ordinary-after-ci"), "{}", b.output);

    ci_hub.stop();
    roles.stop();
    drop(release);
    remove(dir).await;
}
