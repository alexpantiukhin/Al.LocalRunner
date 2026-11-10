mod common;

use common::{base_env, fresh_file, helper_command, pid_alive, real_sleep, workdir, Helper};
use lr_abstractions::{
    Clock, Liveness, ProcessError, ProcessInspector, ProcessRef, ProcessRunner, SecretValue,
    SpawnSpec,
};
use lr_exec::{process_start, ExecRunner, OsProcessInspector, SystemClock};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
const ECHO_TOKEN: &str = "echo [%TOKEN%]> token.txt& ping -n 30 127.0.0.1 >nul";
#[cfg(unix)]
const ECHO_TOKEN: &str = "echo \"[$TOKEN]\" > token.txt; sleep 30";

fn runner() -> ExecRunner {
    ExecRunner::new(Arc::new(SystemClock))
}

fn service(command: &str) -> SpawnSpec {
    let mut spec = SpawnSpec::new(command, workdir("services"));
    spec.env = base_env();
    spec
}

async fn wait_lines(path: &Path, count: usize) -> Vec<(String, u32)> {
    for _ in 0..300 {
        if let Ok(text) = std::fs::read_to_string(path) {
            let lines: Vec<(String, u32)> = text
                .lines()
                .filter_map(|l| {
                    let (role, pid) = l.split_once(' ')?;
                    Some((role.to_string(), pid.trim().parse().ok()?))
                })
                .collect();
            if lines.len() >= count {
                return lines;
            }
        }
        real_sleep(100).await;
    }
    panic!("{} never got {count} lines", path.display());
}

fn pid(lines: &[(String, u32)], role: &str) -> u32 {
    lines.iter().find(|(r, _)| r == role).unwrap().1
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawned_service_tree_is_killed_by_kill_tree() {
    let runner = runner();
    let pids = fresh_file("services", "tree.pids");
    let cmd = helper_command(&Helper::new("tree").pidfile(&pids));
    let process = runner.spawn(service(&cmd)).await.unwrap();
    let start = process.start.expect("service has a start fingerprint");
    let lines = wait_lines(&pids, 2).await;
    let child = pid(&lines, "child");
    let grand = pid(&lines, "grandchild");
    let inspector = OsProcessInspector;
    assert_eq!(inspector.liveness(process.pid, start), Liveness::Alive);
    assert!(pid_alive(child));
    runner.kill_tree(&process).await.unwrap();
    assert!(
        !pid_alive(child),
        "service child {child} survived kill_tree"
    );
    assert!(
        !pid_alive(grand),
        "service grandchild {grand} survived kill_tree"
    );
    assert!(!pid_alive(process.pid));
    assert_eq!(inspector.liveness(process.pid, start), Liveness::Dead);
    runner.kill_tree(&process).await.unwrap();
}

#[tokio::test]
async fn kill_tree_refuses_a_reference_without_fingerprint() {
    let process = ProcessRef {
        pid: std::process::id(),
        start: None,
    };
    assert_eq!(
        runner().kill_tree(&process).await,
        Err(ProcessError::Unverifiable(std::process::id()))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_tree_spares_a_process_with_another_start() {
    let runner = runner();
    let pids = fresh_file("services", "spare.pids");
    let cmd = helper_command(&Helper::new("ignore").pidfile(&pids));
    let process = runner.spawn(service(&cmd)).await.unwrap();
    let child = pid(&wait_lines(&pids, 1).await, "child");
    let reused = ProcessRef {
        pid: process.pid,
        start: Some(process.start.unwrap() + 1),
    };
    runner.kill_tree(&reused).await.unwrap();
    real_sleep(300).await;
    assert!(pid_alive(child), "a reused pid was killed");
    runner.kill_tree(&process).await.unwrap();
    assert!(!pid_alive(child));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orphan_from_an_earlier_run_is_killed_with_its_descendants() {
    let pids = fresh_file("services", "orphan.pids");
    let mut cmd = common::helper::helper_process(&Helper::new("tree").pidfile(&pids));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.stdout(std::process::Stdio::null());
    let mut orphan = cmd.spawn().unwrap();
    let lines = wait_lines(&pids, 2).await;
    let grand = pid(&lines, "grandchild");
    let process = ProcessRef {
        pid: pid(&lines, "child"),
        start: process_start(pid(&lines, "child")),
    };
    assert!(process.start.is_some());
    runner().kill_tree(&process).await.unwrap();
    let _ = orphan.wait();
    assert!(!pid_alive(process.pid));
    assert!(
        !pid_alive(grand),
        "descendant {grand} of the orphan survived"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_gets_its_secrets_in_the_environment() {
    let runner = runner();
    let dir = workdir("services");
    let _ = std::fs::remove_file(dir.join("token.txt"));
    let mut spec = service(ECHO_TOKEN);
    spec.secrets = vec![("TOKEN".to_string(), SecretValue::new("svc-secret"))];
    let process = runner.spawn(spec).await.unwrap();
    let mut text = String::new();
    for _ in 0..100 {
        text = std::fs::read_to_string(dir.join("token.txt")).unwrap_or_default();
        if text.contains(']') {
            break;
        }
        real_sleep(50).await;
    }
    runner.kill_tree(&process).await.unwrap();
    assert!(text.contains("[svc-secret]"), "{text}");
}

#[tokio::test]
async fn spawn_failure_is_reported() {
    let spec = SpawnSpec::new("echo hi", workdir("services").join("missing-dir"));
    assert!(matches!(
        runner().spawn(spec).await,
        Err(ProcessError::Spawn(_))
    ));
}

#[test]
fn inspector_tells_alive_dead_and_unknown() {
    let me = std::process::id();
    let start = process_start(me).expect("own start fingerprint");
    assert_eq!(process_start(me), Some(start));
    let inspector = OsProcessInspector;
    assert_eq!(inspector.liveness(me, start), Liveness::Alive);
    assert_eq!(inspector.liveness(me, start + 1), Liveness::Dead);
    assert_eq!(inspector.liveness(0, start), Liveness::Unknown);
}

#[test]
fn inspector_reports_an_exited_process_dead_before_and_after_reaping() {
    let gate = fresh_file("services", "exit.gate");
    let pids = fresh_file("services", "exit.pids");
    let mut child =
        common::helper::helper_process(&Helper::new("wait-file").gate(&gate).pidfile(&pids))
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
    let pid = child.id();
    let start = process_start(pid).expect("child start");
    let inspector = OsProcessInspector;
    assert_eq!(inspector.liveness(pid, start), Liveness::Alive);
    std::fs::write(&gate, b"go").unwrap();
    let mut state = Liveness::Alive;
    for _ in 0..100 {
        state = inspector.liveness(pid, start);
        if state == Liveness::Dead {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(state, Liveness::Dead, "exited, unreaped child not dead");
    child.wait().unwrap();
    assert_eq!(inspector.liveness(pid, start), Liveness::Dead);
}

#[tokio::test]
async fn system_clock_reads_wall_time_and_sleeps() {
    let clock = SystemClock;
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    assert!(clock.now().0.abs_diff(wall) < 1_000);
    let before = std::time::Instant::now();
    clock.sleep(Duration::from_millis(150)).await;
    assert!(before.elapsed() >= Duration::from_millis(150));
    let before = std::time::Instant::now();
    clock
        .sleep_until(lr_core::Timestamp(clock.now().0 - 5_000))
        .await;
    assert!(before.elapsed() < Duration::from_millis(100));
}

#[test]
fn start_fingerprint_is_the_same_seen_from_the_process_and_from_outside() {
    use std::io::{BufRead, BufReader};
    let gate = fresh_file("services", "start.gate");
    let mut child = common::helper::helper_process(&Helper::new("print-start").gate(&gate))
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut own = String::new();
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
        if line.contains("start-end") {
            own = line;
            break;
        }
    }
    let outside = process_start(child.id());
    std::fs::write(&gate, b"go").unwrap();
    child.wait().unwrap();
    assert!(outside.is_some());
    assert!(
        own.contains(&format!("start={outside:?} ")),
        "inside {own}, outside {outside:?}"
    );
}

#[test]
fn process_exiting_with_code_259_is_dead() {
    let gate = fresh_file("services", "259.gate");
    let mut child =
        common::helper::helper_process(&Helper::new("wait-file").gate(&gate).count(259))
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
    let pid = child.id();
    let start = process_start(pid).unwrap();
    std::fs::write(&gate, b"go").unwrap();
    let mut state = Liveness::Alive;
    for _ in 0..100 {
        state = OsProcessInspector.liveness(pid, start);
        if state == Liveness::Dead {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(state, Liveness::Dead);
    child.wait().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_that_exits_by_itself_leaves_the_registry() {
    let runner = runner();
    let gate = fresh_file("services", "sweep.gate");
    let cmd = helper_command(&Helper::new("wait-file").gate(&gate));
    runner.spawn(service(&cmd)).await.unwrap();
    assert_eq!(runner.service_count(), 1);
    std::fs::write(&gate, b"go").unwrap();
    let mut count = 1;
    for _ in 0..100 {
        count = runner.service_count();
        if count == 0 {
            break;
        }
        real_sleep(50).await;
    }
    assert_eq!(count, 0);
}

#[cfg(windows)]
#[test]
fn soft_stop_that_cannot_be_delivered_is_reported_in_the_job_output() {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::DETACHED_PROCESS;
    let out = common::helper::helper_process(&Helper::new("no-console-cancel"))
        .creation_flags(DETACHED_PROCESS)
        .stdout(std::process::Stdio::piped())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("outcome=Cancelled"), "{text}");
    assert!(text.contains("soft-not-delivered=true"), "{text}");
}
