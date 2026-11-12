mod common;

use common::loopback::{layout, repo, until_ready, until_state};
use common::TempDir;
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_core::{JobId, JobState};
use lr_proto::{ReasonCode, WireSource};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

const GITHUB: [(&str, &str); 9] = [
    ("GITHUB_ACTIONS", "true"),
    ("GITHUB_REPOSITORY", "acme/app"),
    ("GITHUB_WORKFLOW", "checks"),
    ("GITHUB_JOB", "unit"),
    ("GITHUB_RUN_ID", "77"),
    ("GITHUB_RUN_ATTEMPT", "1"),
    ("GITHUB_REF", "refs/heads/main"),
    ("GITHUB_ACTOR", "alex"),
    ("GITHUB_SERVER_URL", "https://github.com"),
];

#[cfg(windows)]
fn shell_running(exe: &str, args: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut c = Command::new("cmd");
    c.raw_arg(format!("/C \"\"{exe}\" {args}\""));
    c
}

#[cfg(not(windows))]
fn shell_running(exe: &str, args: &str) -> Command {
    let mut c = Command::new("sh");
    c.args(["-c", &format!("\"$0\" {args}; true"), exe]);
    c
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workflow_run_whose_step_is_killed_is_cancelled_by_the_reaper_with_sender_gone() {
    let dir = TempDir::new("sender-gone");
    let layout = layout(&dir);
    let repo = repo(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    until_ready(&layout).await;

    let mut step = shell_running(env!("CARGO_BIN_EXE_localrunner"), "run slow --as ci");
    step.current_dir(&repo)
        .env("LOCALRUNNER_STATE_DIR", &layout.root)
        .env("LOCALRUNNER_CONFIG_DIR", &layout.config)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (k, v) in GITHUB {
        step.env(k, v);
    }
    let mut shell = step.spawn().expect("the workflow step starts");
    let stderr = shell.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let mut seen = Vec::new();
    let job = loop {
        let line = rx
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|e| panic!("run prints the job ({e}): {seen:?}"));
        seen.push(line.clone());
        if let Some(id) = line.strip_prefix("job ") {
            break JobId(id.trim().to_string());
        }
    };
    let running = until_state(&layout, &job, JobState::Running).await;
    assert_eq!(running.source, Some(WireSource::Github));
    assert_eq!(
        running.github_run_url.as_deref(),
        Some("https://github.com/acme/app/actions/runs/77/attempts/1")
    );

    shell.kill().expect("the step is killed");
    let _ = shell.wait();

    let done = until_state(&layout, &job, JobState::Cancelled).await;
    assert_eq!(done.summary.reason, Some(ReasonCode::SenderGone));

    runners.stop();
    hub_role.stop();
    broker.stop();
}
