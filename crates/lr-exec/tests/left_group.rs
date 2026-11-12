#![cfg(unix)]

mod common;

use common::{clock, fresh_file, pid_alive, real_sleep};
use lr_abstractions::{ProcessRef, ProcessRunner};
use lr_exec::{containment, process_start, Containment, ExecRunner};
use std::io::Write;
use std::os::unix::process::CommandExt;

struct Left {
    root: u32,
    start: u64,
    left: u32,
}

async fn root_gone_leaving_a_group(name: &str) -> Left {
    let marker = fresh_file("left-group", name);
    let script = format!("sleep 300 & echo $! > '{}'; read _", marker.display());
    let mut root = std::process::Command::new("sh")
        .args(["-c", &script])
        .stdin(std::process::Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = root.id();
    let left: u32 = loop {
        if let Some(p) = std::fs::read_to_string(&marker)
            .ok()
            .and_then(|t| t.trim().parse().ok())
        {
            break p;
        }
        real_sleep(20).await;
    };
    let start = process_start(pid).expect("the root is alive");
    let mut stdin = root.stdin.take().unwrap();
    let _ = stdin.write_all(b"\n");
    drop(stdin);
    root.wait().unwrap();
    assert!(pid_alive(left));
    Left {
        root: pid,
        start,
        left,
    }
}

async fn dies(pid: u32) -> bool {
    for _ in 0..100 {
        if !pid_alive(pid) {
            return true;
        }
        real_sleep(20).await;
    }
    false
}

fn kill(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_tree_of_a_root_gone_after_a_crash_kills_the_group_it_left() {
    let g = root_gone_leaving_a_group("same-boot.pid").await;
    let runner = ExecRunner::new(clock().1);
    let gone = ProcessRef {
        pid: g.root,
        start: Some(g.start),
    };
    runner.kill_tree(&gone).await.unwrap();
    if containment() == Containment::CgroupScope {
        real_sleep(300).await;
        let survived = pid_alive(g.left);
        kill(g.left);
        assert!(
            survived,
            "in a cgroup scope the leftovers are the scope sweep's"
        );
        return;
    }
    assert!(
        dies(g.left).await,
        "the process {} left by the gone root {} survived",
        g.left,
        g.root
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_marker_from_another_boot_leaves_the_group_alone() {
    let g = root_gone_leaving_a_group("other-boot.pid").await;
    let runner = ExecRunner::new(clock().1);
    let from_before = ProcessRef {
        pid: g.root,
        start: Some(0),
    };
    runner.kill_tree(&from_before).await.unwrap();
    real_sleep(300).await;
    let survived = pid_alive(g.left);
    kill(g.left);
    assert!(
        survived,
        "a group named by a marker of another boot is not ours"
    );
}
