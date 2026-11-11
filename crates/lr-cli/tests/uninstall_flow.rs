mod common;

use common::fake_roles::{with_runner, FakeRoles};
use common::TempDir;
use lr_cli::commands::down_cmd::down_with;
use lr_cli::commands::setup::uninstall_with;
use lr_cli::os_service::{Action, System};
use std::future::pending;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Default)]
struct Recorder(Mutex<Vec<Action>>);

impl System for Recorder {
    fn apply(&self, action: &Action) -> Result<(), String> {
        self.0.lock().unwrap().push(action.clone());
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn uninstall_removes_nothing_when_a_runner_outlives_the_30_second_wait() {
    for service in [false, true] {
        let dir = TempDir::new("uninstall-stuck");
        let layout = with_runner(&dir);
        let bin = dir.path().join("bin");
        let system = Recorder::default();
        let roles = FakeRoles::new(service, usize::MAX);
        let start = tokio::time::Instant::now();
        let stop = down_with(&layout, Vec::new(), true, &roles, pending::<()>());
        let code = uninstall_with(&layout, false, false, Some(bin), false, stop, &system).await;
        assert_eq!(code, Ok(1), "service broker: {service}");
        let took = start.elapsed();
        assert!(
            took >= Duration::from_secs(30) && took < Duration::from_secs(31),
            "{took:?}"
        );
        assert_eq!(roles.broker_stops(), 0, "the broker keeps running");
        assert!(
            system.0.lock().unwrap().is_empty(),
            "no autostart entry, service, binary or PATH entry is touched"
        );
    }
}

#[tokio::test]
async fn uninstall_after_every_role_stopped_removes_autostart_binary_and_path_entry() {
    let dir = TempDir::new("uninstall-go");
    let layout = with_runner(&dir);
    let bin = dir.path().join("bin");
    let system = Recorder::default();
    let roles = FakeRoles::new(false, 0);
    let stopped = down_with(&layout, Vec::new(), true, &roles, pending::<()>());
    uninstall_with(
        &layout,
        false,
        false,
        Some(bin.clone()),
        false,
        stopped,
        &system,
    )
    .await
    .unwrap();
    assert_eq!(roles.broker_stops(), 1);
    let applied = system.0.lock().unwrap().clone();
    assert!(
        applied.contains(&Action::RemoveFromUserPath(bin.clone())),
        "{applied:?}"
    );
    assert!(
        applied
            .iter()
            .any(|a| matches!(a, Action::RemoveFile(p) if p.starts_with(&bin))),
        "{applied:?}"
    );
    assert!(
        !applied.iter().any(|a| matches!(a, Action::RemoveDirAll(_))),
        "without --purge the state directory stays: {applied:?}"
    );

    let refused = Recorder::default();
    let failed = async { Err("no principal".to_string()) };
    let code = uninstall_with(&layout, false, false, Some(bin), false, failed, &refused).await;
    assert_eq!(code, Ok(1));
    assert!(refused.0.lock().unwrap().is_empty());
}
