mod common;

use common::TempDir;
use lr_abstractions::fakes::FakeProcessInspector;
use lr_abstractions::ProcessRef;
use lr_cli::paths::Layout;
use lr_cli::upgrade_lock::{blocking, describe, live, lock_path, write, write_with, UpgradeLock};
use lr_core::Timestamp;

fn layout(dir: &TempDir) -> Layout {
    Layout::new(dir.path().join("state"), dir.path().join("config"))
}

fn lock(pid: u32, start: u64) -> UpgradeLock {
    UpgradeLock {
        pid,
        start,
        target: "0.5.0".into(),
        since: Timestamp(1_700_000_000_000),
    }
}

fn running(pid: u32, start: u64) -> FakeProcessInspector {
    let table = FakeProcessInspector::new();
    table.add(ProcessRef {
        pid,
        start: Some(start),
    });
    table
}

#[test]
fn live_returns_none_for_a_missing_file() {
    let dir = TempDir::new("upgrade_lock");
    assert_eq!(live(&layout(&dir), &FakeProcessInspector::new()), None);
}

#[test]
fn live_returns_none_for_garbage() {
    let dir = TempDir::new("upgrade_lock");
    let l = layout(&dir);
    std::fs::create_dir_all(&l.root).unwrap();
    std::fs::write(lock_path(&l), "not json").unwrap();
    assert_eq!(live(&l, &running(1, 1)), None);
}

#[test]
fn live_returns_none_for_a_dead_process() {
    let dir = TempDir::new("upgrade_lock");
    let l = layout(&dir);
    let _guard = write(&l, &lock(4321, 7)).unwrap();
    assert_eq!(live(&l, &FakeProcessInspector::new()), None);
}

#[test]
fn live_returns_none_when_the_pid_was_reused() {
    let dir = TempDir::new("upgrade_lock");
    let l = layout(&dir);
    let _guard = write(&l, &lock(4321, 7)).unwrap();
    assert_eq!(live(&l, &running(4321, 8)), None);
}

#[test]
fn live_returns_the_lock_for_a_live_process() {
    let dir = TempDir::new("upgrade_lock");
    let l = layout(&dir);
    let _guard = write(&l, &lock(4321, 7)).unwrap();
    assert_eq!(live(&l, &running(4321, 7)), Some(lock(4321, 7)));
}

#[test]
fn the_guard_removes_the_file_on_drop() {
    let dir = TempDir::new("upgrade_lock");
    let l = layout(&dir);
    let guard = write(&l, &lock(4321, 7)).unwrap();
    assert!(lock_path(&l).exists());
    drop(guard);
    assert!(!lock_path(&l).exists());
}

#[test]
fn the_owner_and_its_children_are_not_blocked() {
    let dir = TempDir::new("upgrade_lock");
    let l = layout(&dir);
    let _guard = write(&l, &lock(4321, 7)).unwrap();
    let table = running(4321, 7);
    assert!(blocking(&l, &table, 99, None).is_some());
    assert!(blocking(&l, &table, 4321, None).is_none());
    assert!(blocking(&l, &table, 99, Some(4321)).is_none());
    assert!(blocking(&l, &table, 99, Some(5)).is_some());
}

#[test]
fn describe_names_the_target_the_pid_and_the_time() {
    assert_eq!(
        describe(&lock(4321, 7)),
        "upgrade to 0.5.0 runs (pid 4321, since 2023-11-14 22:13:20Z)"
    );
}

fn long_running_child() -> std::process::Child {
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("ping");
        c.args(["-n", "60", "127.0.0.1"]);
        c
    } else {
        let mut c = std::process::Command::new("sleep");
        c.arg("60");
        c
    };
    cmd.stdout(std::process::Stdio::null()).spawn().unwrap()
}

#[tokio::test]
async fn up_refuses_while_a_live_lock_of_another_process_exists() {
    let dir = TempDir::new("upgrade_lock_up");
    let l = layout(&dir);
    let mut child = long_running_child();
    let other = UpgradeLock {
        pid: child.id(),
        start: lr_exec::process_start(child.id()).unwrap(),
        target: "0.5.0".into(),
        since: Timestamp(1),
    };
    let _guard = write(&l, &other).unwrap();
    let start = |_: &[String]| -> std::io::Result<u32> { panic!("no role may start") };
    let code =
        lr_cli::commands::up_cmd::up_with(&l, None, std::time::Duration::from_secs(1), &start)
            .await;
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(code, Ok(1));
}

#[test]
fn a_second_upgrade_is_refused_while_the_first_lock_is_live() {
    let dir = TempDir::new("upgrade_lock");
    let l = layout(&dir);
    let _first = write_with(&l, &lock(10, 1), &running(10, 1)).unwrap();
    let refused = write_with(&l, &lock(20, 2), &running(10, 1)).unwrap_err();
    assert!(
        refused.contains("upgrade to 0.5.0 runs (pid 10"),
        "{refused}"
    );
    assert_eq!(live(&l, &running(10, 1)).map(|k| k.pid), Some(10));
}

#[test]
fn a_dead_lock_is_replaced_by_the_next_upgrade() {
    let dir = TempDir::new("upgrade_lock");
    let l = layout(&dir);
    std::fs::create_dir_all(&l.root).unwrap();
    std::fs::write(lock_path(&l), serde_json::to_string(&lock(10, 1)).unwrap()).unwrap();
    let _guard = write_with(&l, &lock(20, 2), &FakeProcessInspector::new()).unwrap();
    assert_eq!(live(&l, &running(20, 2)).map(|k| k.pid), Some(20));
}

#[test]
fn a_guard_does_not_remove_the_lock_of_another_process() {
    let dir = TempDir::new("upgrade_lock");
    let l = layout(&dir);
    let guard = write_with(&l, &lock(10, 1), &FakeProcessInspector::new()).unwrap();
    std::fs::write(lock_path(&l), serde_json::to_string(&lock(20, 2)).unwrap()).unwrap();
    drop(guard);
    assert_eq!(live(&l, &running(20, 2)).map(|k| k.pid), Some(20));
}
