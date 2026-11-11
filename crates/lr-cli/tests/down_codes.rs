mod common;

use common::fake_roles::{with_runner, FakeRoles};
use common::TempDir;
use lr_cli::commands::down_cmd::{down, down_with};
use lr_cli::commands::up_cmd::{service_broker, startable};
use lr_cli::init::NodeRole;
use lr_cli::paths::Layout;
use lr_proto::DownMode;
use std::future::pending;
use std::time::Duration;

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

fn within_30_to_31_s(start: tokio::time::Instant) {
    let took = start.elapsed();
    assert!(
        took >= Duration::from_secs(30) && took < Duration::from_secs(31),
        "{took:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn down_cancel_with_a_runner_still_running_after_30_s_exits_1_and_leaves_the_broker() {
    let dir = TempDir::new("down-stuck");
    let layout = with_runner(&dir);
    let roles = FakeRoles::new(false, usize::MAX);
    let start = tokio::time::Instant::now();
    let code = down_with(&layout, Vec::new(), true, &roles, pending::<()>()).await;
    assert_eq!(code, Ok(1));
    within_30_to_31_s(start);
    assert_eq!(
        *roles.told.lock().unwrap(),
        vec![("r1".to_string(), DownMode::Cancel)]
    );
    assert_eq!(roles.broker_stops(), 0, "the broker keeps running");
}

#[tokio::test(start_paused = true)]
async fn down_cancel_waits_30_s_for_the_runners_also_before_a_broker_service() {
    let dir = TempDir::new("down-stuck-service");
    let layout = with_runner(&dir);
    let roles = FakeRoles::new(true, usize::MAX);
    let start = tokio::time::Instant::now();
    let code = down_with(&layout, Vec::new(), true, &roles, pending::<()>()).await;
    assert_eq!(code, Ok(1), "a runner that did not exit is a failed down");
    within_30_to_31_s(start);
    assert_eq!(roles.broker_stops(), 0);

    let gone = FakeRoles::new(true, 2);
    let code = down_with(&layout, Vec::new(), true, &gone, pending::<()>()).await;
    assert_eq!(
        code,
        Ok(0),
        "runners gone: the service broker says it stays"
    );
    assert_eq!(gone.broker_stops(), 1);
}

#[tokio::test(start_paused = true)]
async fn down_drain_waits_until_ctrl_c_with_no_time_limit_and_then_exits_1() {
    let dir = TempDir::new("down-drain");
    let layout = with_runner(&dir);
    let roles = FakeRoles::new(false, usize::MAX);
    let start = tokio::time::Instant::now();
    let ctrl_c = tokio::time::sleep(Duration::from_secs(600));
    let code = down_with(&layout, Vec::new(), false, &roles, ctrl_c).await;
    assert_eq!(code, Ok(1));
    assert!(start.elapsed() >= Duration::from_secs(600));
    assert_eq!(
        *roles.told.lock().unwrap(),
        vec![("r1".to_string(), DownMode::Drain)]
    );
    assert_eq!(roles.broker_stops(), 0, "the broker keeps running");

    let finishing = FakeRoles::new(false, 3);
    let code = down_with(&layout, Vec::new(), false, &finishing, pending::<()>()).await;
    assert_eq!(code, Ok(0));
    assert_eq!(
        finishing.broker_stops(),
        1,
        "the broker stops after its runners"
    );
}
