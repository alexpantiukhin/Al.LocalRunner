mod common;

use common::TempDir;
use lr_cli::commands::down_cmd::{wait_for_runners, Waited};
use lr_cli::commands::setup::uninstall_with;
use lr_cli::os_service::{Action, System};
use lr_cli::paths::Layout;
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

fn layout(dir: &TempDir) -> Layout {
    Layout::new(dir.path().join("state"), dir.path().join("config"))
}

#[tokio::test(start_paused = true)]
async fn uninstall_removes_nothing_when_a_runner_outlives_the_30_second_wait() {
    let dir = TempDir::new("uninstall-stuck");
    let layout = layout(&dir);
    let bin = dir.path().join("bin");
    let system = Recorder::default();
    let start = tokio::time::Instant::now();
    let stuck_runner = async {
        let waited = wait_for_runners(|| async { true }, true, std::future::pending::<()>()).await;
        assert_eq!(waited, Waited::TimedOut);
        Ok(1)
    };
    let code = uninstall_with(
        &layout,
        false,
        false,
        Some(bin),
        false,
        stuck_runner,
        &system,
    )
    .await;
    assert_eq!(code, Ok(1));
    assert!(start.elapsed() >= Duration::from_secs(30));
    assert!(
        system.0.lock().unwrap().is_empty(),
        "no autostart entry, service, binary or PATH entry is touched"
    );
}

#[tokio::test]
async fn uninstall_after_every_role_stopped_removes_autostart_binary_and_path_entry() {
    let dir = TempDir::new("uninstall-go");
    let layout = layout(&dir);
    let bin = dir.path().join("bin");
    let system = Recorder::default();
    let stopped = async { Ok(0) };
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
