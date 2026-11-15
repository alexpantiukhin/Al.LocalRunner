mod common;

use common::loopback::layout;
use common::TempDir;
use lr_cli::roles::{start_broker, start_runners};
use lr_cli::upgrade_lock::{lock_path, UpgradeLock};
use lr_runner::AgentExit;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn long_lived() -> Child {
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("ping");
        c.args(["-n", "120", "127.0.0.1"]);
        c
    } else {
        let mut c = Command::new("sleep");
        c.arg("120");
        c
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("a long-lived process starts")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_runner_assembled_under_a_live_upgrade_lock_starts_in_drain_and_leaves_once_idle() {
    let dir = TempDir::new("runner-under-lock");
    let layout = layout(&dir);
    let mut upgrade = long_lived();
    let lock = UpgradeLock {
        pid: upgrade.id(),
        start: lr_exec::process_start(upgrade.id()).unwrap_or(0),
        target: "9.9.9".into(),
        since: lr_core::Timestamp(1_700_000_000_000),
    };
    std::fs::create_dir_all(&layout.root).unwrap();
    std::fs::write(lock_path(&layout), serde_json::to_string(&lock).unwrap()).unwrap();
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    let exits = tokio::time::timeout(Duration::from_secs(60), runners.wait())
        .await
        .expect("a runner started under a live lock drains and leaves");
    assert_eq!(exits.len(), 1);
    assert!(matches!(exits[0], AgentExit::Stopped), "{:?}", exits[0]);
    let _ = upgrade.kill();
    let _ = upgrade.wait();
    broker.stop();
}
