#![cfg(unix)]

mod common;

use common::{clock, helper_command, Helper, Link, Links};
use lr_abstractions::{ProcessRef, ProcessRunner};
use lr_exec::{containment, process_start, Containment, ExecRunner};
use std::io::Write;
use std::os::unix::process::CommandExt;

struct Left {
    root: u32,
    start: u64,
    left: Link,
}

async fn root_gone_leaving_a_group() -> Left {
    let links = Links::new().await;
    let pong = helper_command(&Helper::new("pong").link(&links));
    let script = format!("{pong} >/dev/null 2>&1 & read _");
    let mut root = std::process::Command::new("sh")
        .args(["-c", &script])
        .stdin(std::process::Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = root.id();
    let mut left = links.next().await;
    let start = process_start(pid).expect("the root is alive");
    let mut stdin = root.stdin.take().unwrap();
    let _ = stdin.write_all(b"\n");
    drop(stdin);
    root.wait().unwrap();
    assert!(left.ping().await, "the process left by the root answers");
    Left {
        root: pid,
        start,
        left,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_tree_of_a_root_gone_after_a_crash_kills_the_group_it_left() {
    let mut g = root_gone_leaving_a_group().await;
    let runner = ExecRunner::new(clock().1);
    let gone = ProcessRef {
        pid: g.root,
        start: Some(g.start),
    };
    runner.kill_tree(&gone).await.unwrap();
    if containment() == Containment::CgroupScope {
        assert!(
            g.left.ping().await,
            "in a cgroup scope the leftovers are the scope sweep's"
        );
        return;
    }
    g.left.gone().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_marker_from_another_boot_leaves_the_group_alone() {
    let mut g = root_gone_leaving_a_group().await;
    let runner = ExecRunner::new(clock().1);
    let from_before = ProcessRef {
        pid: g.root,
        start: Some(0),
    };
    runner.kill_tree(&from_before).await.unwrap();
    assert!(
        g.left.ping().await,
        "a group named by a marker of another boot is not ours"
    );
}
