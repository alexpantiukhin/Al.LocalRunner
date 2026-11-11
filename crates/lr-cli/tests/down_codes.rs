mod common;

use common::TempDir;
use lr_cli::commands::down_cmd::{down, wait_for_runners, Waited};
use lr_cli::commands::up_cmd::{service_broker, startable};
use lr_cli::init::NodeRole;
use lr_cli::paths::Layout;

fn layout(dir: &TempDir) -> Layout {
    let root = dir.path().join("state");
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    Layout::new(root, config)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn down_of_a_named_role_that_does_not_run_exits_with_1_and_a_full_down_with_0() {
    let dir = TempDir::new("down-codes");
    let layout = layout(&dir);
    if layout.broker_is_service() {
        eprintln!("SKIPPED: the broker service of this machine answers for the default layout");
        return;
    }
    assert_eq!(down(&layout, vec!["hub".into()], false).await, Ok(1));
    assert_eq!(down(&layout, vec!["broker".into()], true).await, Ok(1));
    assert_eq!(down(&layout, vec!["r9".into()], false).await, Ok(1));
    assert_eq!(down(&layout, Vec::new(), false).await, Ok(0));
}

#[test]
fn up_leaves_a_stopped_broker_service_to_the_service_manager_with_exit_code_1() {
    assert!(startable(vec![NodeRole::Broker], true).is_empty());
    assert_eq!(service_broker(true), Ok("broker: running as an OS service"));
    let stopped = service_broker(false).unwrap_err();
    assert!(stopped.contains("not running"), "{stopped}");
    assert!(stopped.contains("OS service manager"), "{stopped}");
}

#[tokio::test(start_paused = true)]
async fn after_cancel_down_waits_for_runners_30_seconds_and_no_longer() {
    let start = tokio::time::Instant::now();
    let never = std::future::pending::<()>();
    let waited = wait_for_runners(|| async { true }, true, never).await;
    assert_eq!(waited, Waited::TimedOut);
    let took = start.elapsed();
    assert!(
        took >= std::time::Duration::from_secs(30) && took < std::time::Duration::from_secs(31),
        "{took:?}"
    );

    let mut left = 3;
    let waited = wait_for_runners(
        move || {
            left -= 1;
            async move { left > 0 }
        },
        true,
        std::future::pending::<()>(),
    )
    .await;
    assert_eq!(waited, Waited::Gone);

    let start = tokio::time::Instant::now();
    let ctrl_c = tokio::time::sleep(std::time::Duration::from_secs(600));
    let waited = wait_for_runners(|| async { true }, false, ctrl_c).await;
    assert_eq!(waited, Waited::Interrupted, "a drain has no time limit");
    assert!(start.elapsed() >= std::time::Duration::from_secs(600));
}
