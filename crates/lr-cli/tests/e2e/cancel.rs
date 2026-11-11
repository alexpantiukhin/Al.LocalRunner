use crate::common::TempDir;
use crate::helper::{helper_command, Mode};
use crate::kit::*;
use crate::support::*;
use lr_cli::commands::jobs::cancel;
use lr_core::JobState;
use lr_proto::ReasonCode;
use std::time::Duration;

const ENDS: Duration = Duration::from_secs(60);

// scenario: 16.17
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_of_a_running_job_kills_the_tree_and_frees_the_pool() {
    let dir = TempDir::new("e2e-cnl");
    let gate = dir.path().join("gate");
    let mut release = Release::new(&[&gate]);
    let (main, roles) = machine(&dir, "main", "", &["r1"], &runner_toml()).await;
    let (guest, guest_hub) = joined_hub(&dir, &main, "guest", "guest", None).await;
    let tree = format!(
        "[profile.tree]\ncommand = {}\ntimeout_s = 120\nmem_mb = 256\n",
        toml_str(&helper_command(
            Mode::Tree,
            &pid_file(dir.path(), "tree"),
            &gate
        ))
    );
    let profiles = format!(
        "{tree}\n{}",
        echo_profile("behind", "never-runs", 60, "mem_mb = 256\n")
    );
    let repo = repo(&dir, &profiles);

    let (tree_client, running) = submit(&main, &repo, "tree", None).await;
    let processes = [
        started(dir.path(), "tree", "child").await,
        started(dir.path(), "tree", "grandchild").await,
    ];
    for p in processes {
        release.watch(p);
    }
    let (behind_client, waiting) = submit(&main, &repo, "behind", None).await;
    queued(&main, &waiting).await;

    let foreign = cancel(&guest, Some(running.0.clone()), LABEL, false, true, None).await;
    assert!(
        !matches!(foreign, Ok(0)),
        "a hub of another key cannot cancel the order: {foreign:?}"
    );
    let view = status(&main).await.expect("status");
    assert!(
        view.running.iter().any(|r| r.summary.job == running),
        "{view:?}"
    );

    let code = cancel(&main, Some(waiting.0.clone()), LABEL, false, false, None)
        .await
        .expect("cancel of the waiting order");
    assert_eq!(code, 0);
    let ended = until_exit(behind_client, &waiting, ENDS).await;
    assert_eq!(ended.state, JobState::Cancelled, "{ended:?}");
    assert_eq!(ended.reason, Some(ReasonCode::CancelledByUser), "{ended:?}");
    let removed = row(&main, &waiting).await;
    assert!(
        removed.started_at.is_none(),
        "taken off the queue at once: {removed:?}"
    );

    let code = cancel(&main, Some(running.0.clone()), LABEL, false, false, None)
        .await
        .expect("cancel of the running job");
    assert_eq!(code, 0);
    let ended = until_exit(tree_client, &running, ENDS).await;
    assert_eq!(ended.state, JobState::Cancelled, "{ended:?}");
    assert_eq!(ended.reason, Some(ReasonCode::CancelledByUser), "{ended:?}");
    gone("the job tree to be killed", &processes).await;
    let view = status(&main).await.expect("status");
    assert_eq!(view.pool.used_mem_mb, 0, "the pool is free: {view:?}");
    assert!(view.running.is_empty(), "{view:?}");

    guest_hub.stop();
    roles.stop();
    drop(release);
    remove(dir).await;
}
