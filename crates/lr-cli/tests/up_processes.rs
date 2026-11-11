mod common;

use common::TempDir;
use lr_cli::commands::up_cmd::running_roles;
use lr_cli::paths::Layout;
use std::collections::BTreeSet;
use std::process::Command;
use std::time::Duration;

fn layout(dir: &TempDir) -> Layout {
    let root = dir.path().join("state");
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    let user = lr_net::current_os_user().unwrap();
    std::fs::write(
        config.join("policy.toml"),
        format!("[[local_user]]\nname = {user:?}\n"),
    )
    .unwrap();
    let runner = root.join("runner").join("r1");
    std::fs::create_dir_all(&runner).unwrap();
    std::fs::write(
        runner.join("runner.toml"),
        format!(
            "allow_elevated = true\nmem_mb = 1024\ncpu = 1\n{}",
            common::env_path_toml()
        ),
    )
    .unwrap();
    Layout::new(root, config)
}

fn localrunner(layout: &Layout, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_localrunner"))
        .args(args)
        .arg("--state-dir")
        .arg(&layout.root)
        .arg("--config-dir")
        .arg(&layout.config)
        .output()
        .expect("the binary runs")
}

struct StopOnDrop(Layout);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        let _ = localrunner(&self.0, &["down"]);
    }
}

fn pids(stdout: &str) -> Vec<(String, u32)> {
    stdout
        .lines()
        .filter_map(|l| {
            let (role, rest) = l.split_once(": started (pid ")?;
            Some((role.to_string(), rest.trim_end_matches(')').parse().ok()?))
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn up_starts_the_broker_the_hub_and_the_runner_as_separate_processes() {
    let dir = TempDir::new("up-processes");
    let layout = layout(&dir);
    let _stop = StopOnDrop(layout.clone());
    let up = tokio::process::Command::new(env!("CARGO_BIN_EXE_localrunner"))
        .args(["up", "all", "--state-dir"])
        .arg(&layout.root)
        .arg("--config-dir")
        .arg(&layout.config)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("the binary runs");
    let up = tokio::time::timeout(Duration::from_secs(60), up.wait_with_output())
        .await
        .expect("up returns while the roles keep running: they hold none of its handles")
        .expect("up output");
    let stdout = String::from_utf8_lossy(&up.stdout).into_owned();
    let started = pids(&stdout);
    let roles: Vec<&str> = started.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(
        roles,
        ["broker run", "hub run", "runner run"],
        "{stdout}\n{}",
        String::from_utf8_lossy(&up.stderr)
    );
    let distinct: BTreeSet<u32> = started.iter().map(|(_, p)| *p).collect();
    assert_eq!(distinct.len(), 3, "one process per role: {started:?}");
    assert!(!distinct.contains(&std::process::id()));
    let now = running_roles(&layout).await;
    assert!(
        now.broker && now.hub && now.runners.contains("r1"),
        "{now:?}"
    );
    let down = localrunner(&layout, &["down"]);
    assert_eq!(
        down.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
    for _ in 0..100 {
        let gone = running_roles(&layout).await;
        if !gone.broker && !gone.hub && gone.runners.is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the roles still answer after down");
}
