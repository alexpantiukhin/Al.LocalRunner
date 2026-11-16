mod common;

use common::{base_env, exited, helper_command, pid_alive, workdir, Helper, Links};
use lr_abstractions::{
    Clock, Liveness, ProcessError, ProcessInspector, ProcessRef, ProcessRunner, SecretValue,
    SpawnSpec,
};
use lr_exec::{process_start, ExecRunner, OsProcessInspector, SystemClock};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn runner() -> ExecRunner {
    ExecRunner::new(Arc::new(SystemClock))
}

fn service(command: &str) -> SpawnSpec {
    let mut spec = SpawnSpec::new(command, workdir("services"));
    spec.env = base_env();
    spec
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawned_service_tree_is_killed_by_kill_tree() {
    let runner = runner();
    let links = Links::new().await;
    let cmd = helper_command(&Helper::new("tree").link(&links));
    let process = runner.spawn(service(&cmd)).await.unwrap();
    let start = process.start.expect("service has a start fingerprint");
    let tree = links.roles(&["child", "grandchild"]).await;
    let (child, grand) = (tree[0].pid, tree[1].pid);
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
    let links = Links::new().await;
    let cmd = helper_command(&Helper::new("ignore").link(&links));
    let process = runner.spawn(service(&cmd)).await.unwrap();
    let child = links.next().await.pid;
    let reused = ProcessRef {
        pid: process.pid,
        start: Some(process.start.unwrap() + 1),
    };
    runner.kill_tree(&reused).await.unwrap();
    assert!(pid_alive(child), "a reused pid was killed");
    runner.kill_tree(&process).await.unwrap();
    assert!(!pid_alive(child));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orphan_from_an_earlier_run_is_killed_with_its_descendants() {
    let links = Links::new().await;
    let mut cmd = common::helper::helper_process(&Helper::new("tree").link(&links));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.stdout(std::process::Stdio::null());
    let mut orphan = cmd.spawn().unwrap();
    let tree = links.roles(&["child", "grandchild"]).await;
    let grand = tree[1].pid;
    let process = ProcessRef {
        pid: tree[0].pid,
        start: process_start(tree[0].pid),
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
    let links = Links::new().await;
    let cmd = helper_command(&Helper::new("ignore").link(&links).show("TOKEN"));
    let mut spec = service(&cmd);
    spec.secrets = vec![("TOKEN".to_string(), SecretValue::new("svc-secret"))];
    let process = runner.spawn(spec).await.unwrap();
    let shown = links.next().await.shown;
    runner.kill_tree(&process).await.unwrap();
    assert_eq!(shown, "TOKEN=[svc-secret]");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inspector_reports_an_exited_process_dead_before_and_after_reaping() {
    let links = Links::new().await;
    let mut child = common::helper::helper_process(&Helper::new("wait-gate").link(&links))
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let mut gate = links.next().await;
    let start = process_start(pid).expect("child start");
    let inspector = OsProcessInspector;
    assert_eq!(inspector.liveness(pid, start), Liveness::Alive);
    gate.open().await;
    exited(pid).await;
    assert_eq!(
        inspector.liveness(pid, start),
        Liveness::Dead,
        "exited, unreaped child not dead"
    );
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_fingerprint_is_the_same_seen_from_the_process_and_from_outside() {
    use std::io::{BufRead, BufReader};
    let links = Links::new().await;
    let mut child = common::helper::helper_process(&Helper::new("print-start").link(&links))
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut gate = links.next().await;
    let mut own = String::new();
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
        if line.contains("start-end") {
            own = line;
            break;
        }
    }
    let outside = process_start(child.id());
    gate.open().await;
    child.wait().unwrap();
    assert!(outside.is_some());
    assert!(
        own.contains(&format!("start={outside:?} ")),
        "inside {own}, outside {outside:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_exiting_with_code_259_is_dead() {
    let links = Links::new().await;
    let mut child =
        common::helper::helper_process(&Helper::new("wait-gate").link(&links).count(259))
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
    let pid = child.id();
    let mut gate = links.next().await;
    let start = process_start(pid).unwrap();
    gate.open().await;
    exited(pid).await;
    assert_eq!(OsProcessInspector.liveness(pid, start), Liveness::Dead);
    child.wait().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_that_exits_by_itself_leaves_the_registry() {
    let runner = runner();
    let links = Links::new().await;
    let cmd = helper_command(&Helper::new("wait-gate").link(&links));
    let process = runner.spawn(service(&cmd)).await.unwrap();
    let mut gate = links.next().await;
    assert_eq!(runner.service_count(), 1);
    gate.open().await;
    exited(process.pid).await;
    assert_eq!(runner.service_count(), 0);
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

#[cfg(windows)]
#[test]
fn runner_started_without_a_console_gets_a_hidden_one_and_soft_stop_reaches_the_step() {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::DETACHED_PROCESS;
    let out = common::helper::helper_process(&Helper::new("no-console-hosted"))
        .creation_flags(DETACHED_PROCESS)
        .stdout(std::process::Stdio::piped())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("console-created=true"), "{text}");
    assert!(text.contains("outcome=Cancelled"), "{text}");
    assert!(text.contains("soft-not-delivered=false"), "{text}");
    assert!(text.contains("got-soft=true"), "{text}");
}
