use crate::common::TempDir;
use crate::support::*;
use lr_cli::commands::jobs::hub;
use lr_cli::commands::log_cmd::{find_log, LogQuery};
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_core::JobState;
use lr_obs::Redactor;
use std::sync::Arc;
use std::time::Duration;

const MASKED_LINE: &str = "e2e-out [***] done";

fn echo_secret() -> String {
    let var = SECRET_NAME.to_uppercase();
    if cfg!(windows) {
        format!("echo e2e-out [%{var}%] done")
    } else {
        format!("echo \"e2e-out [${var}] done\"")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn single_machine_submit_wait_passes_and_history_has_one_row() {
    let dir = TempDir::new("e2e-single");
    let layout = layout(&dir, "machine", "");
    add_runner(&layout, "r1");
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    runner_online(&layout, "r1").await;

    let profiles = format!(
        "[profile.unit]\ncommand = {}\ntimeout_s = 60\nmem_mb = 256\nsecrets = [\"{SECRET_NAME}\"]\n",
        toml_str(&echo_secret())
    );
    let repo = repo(&dir, &profiles);
    let (client, job) = submit(&layout, &repo, "unit", None).await;
    let ended = until_exit(client, &job, Duration::from_secs(60)).await;
    assert_eq!(ended.state, JobState::Passed, "{ended:?}");
    assert_eq!(ended.exit_code, Some(0), "{ended:?}");
    assert!(
        ended.output.contains(MASKED_LINE),
        "the secret is replaced by the mask: {}",
        ended.output
    );
    assert!(
        !ended.output.contains(SECRET_VALUE),
        "the runner masks the secret before it leaves: {}",
        ended.output
    );

    let rows = history(&layout).await;
    assert_eq!(rows.len(), 1, "one submit is one run: {rows:?}");
    let row = &rows[0];
    assert_eq!(row.job, job);
    assert_eq!(row.state, JobState::Passed);
    assert_eq!(row.exit_code, Some(0));
    assert!(row.runner.as_deref().is_some_and(|r| r.ends_with("r1")));

    let client = hub(&layout).await.expect("hub channel");
    let query = LogQuery {
        job: job.clone(),
        step: None,
        tail: None,
        follow: false,
    };
    let mut log = String::new();
    find_log(
        &client,
        None,
        &query,
        &Arc::new(Redactor::new(&[])),
        &mut |t| log.push_str(t),
    )
    .await
    .expect("stored log");
    client.close().await;
    assert!(log.contains(MASKED_LINE), "{log}");
    assert!(
        !log.contains(SECRET_VALUE),
        "the stored log is masked: {log}"
    );

    runners.stop();
    hub_role.stop();
    broker.stop();
    remove(dir).await;
}
