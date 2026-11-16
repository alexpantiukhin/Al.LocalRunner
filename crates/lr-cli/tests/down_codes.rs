mod common;

use common::fake_roles::{with_runner, FakeRoles};
use common::TempDir;
use lr_cli::commands::down_cmd::{down, down_with, service_state, ServiceState};
use lr_cli::commands::up_cmd::{service_broker, startable};
use lr_cli::init::NodeRole;
use lr_cli::os_service::{
    state::{restart_command, status_command},
    Os,
};
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

#[tokio::test(start_paused = true)]
async fn down_names_a_runner_that_runs_as_an_os_service_instead_of_calling_it_stopped() {
    let dir = TempDir::new("down-service-runner");
    let layout = with_runner(&dir);
    let mut roles = FakeRoles::new(false, 0);
    roles.service_runner = Some(Some(true));
    roles.service_unreachable = true;
    let code = down_with(&layout, vec!["r1".into()], false, &roles, pending::<()>()).await;
    assert_eq!(
        code,
        Ok(1),
        "the runner keeps running: down did not stop it"
    );
    let err = roles.said_err().join("\n");
    assert!(
        err.contains("runner r1 runs as the OS service") && err.contains("down does not reach it"),
        "{err}"
    );
    assert!(!err.contains("not running"), "{err}");
    let mut all = FakeRoles::new(false, 0);
    all.service_runner = Some(Some(true));
    all.service_unreachable = true;
    assert_eq!(
        down_with(&layout, Vec::new(), false, &all, pending::<()>()).await,
        Ok(1),
        "a full down that leaves a runner running says so in its exit code"
    );
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

#[tokio::test(start_paused = true)]
async fn down_of_a_runner_alone_reports_stopped_and_for_a_slow_one_still_running() {
    let dir = TempDir::new("down-named");
    let layout = with_runner(&dir);
    let gone = FakeRoles::new(false, 2);
    let start = tokio::time::Instant::now();
    let code = down_with(&layout, vec!["r1".into()], false, &gone, pending::<()>()).await;
    assert_eq!(code, Ok(0));
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(gone.broker_stops(), 0);

    let busy = FakeRoles::new(false, usize::MAX);
    let start = tokio::time::Instant::now();
    let code = down_with(&layout, vec!["r1".into()], false, &busy, pending::<()>()).await;
    assert_eq!(
        code,
        Ok(0),
        "a runner finishing its job is not a failed down"
    );
    let took = start.elapsed();
    assert!(
        took >= Duration::from_secs(3) && took < Duration::from_secs(4),
        "{took:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_runner_with_no_job_that_does_not_stop_is_not_told_to_wait_for_a_job() {
    let dir = TempDir::new("down-stuck-idle");
    let layout = with_runner(&dir);
    let idle = FakeRoles::new(false, usize::MAX);
    *idle.jobs.lock().unwrap() = Some(vec!["job-9 on r2 (rust-unit), running 1 s".into()]);
    let code = down_with(&layout, vec!["r1".into()], false, &idle, pending::<()>()).await;
    assert_eq!(code, Ok(0));
    let said = idle.said();
    assert_eq!(
        said,
        vec!["r1: still running with no job: it is delivering its outbox or stopping its services; it leaves on its own within 10 s of the stop, or once its services have stopped (what it could not deliver is sent on its next start)".to_string()]
    );
    assert!(!said[0].contains("stuck") && !said[0].contains("--cancel"));

    let busy = FakeRoles::new(false, usize::MAX);
    *busy.jobs.lock().unwrap() = Some(vec!["job-1 on r1 (rust-unit), running 1 s".into()]);
    down_with(&layout, vec!["r1".into()], false, &busy, pending::<()>())
        .await
        .unwrap();
    assert_eq!(
        busy.said(),
        vec![
            "r1: still running, it stops when its job ends (`localrunner status` shows the job)"
                .to_string()
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn down_broker_names_a_running_service_it_cannot_reach() {
    let dir = TempDir::new("down-service");
    let layout = layout(&dir);
    let roles = FakeRoles::unreachable_broker(ServiceState::Answers);
    let code = down_with(
        &layout,
        vec!["broker".into()],
        false,
        &roles,
        pending::<()>(),
    )
    .await;
    assert_eq!(code, Ok(1));
    let said = roles.said_err().join("\n");
    let want = format!(
        "broker: running as an OS service; its admin channel is open only to an administrator, so down cannot stop it from here. Restart it from an elevated shell: {}",
        restart_command(Os::current())
    );
    assert_eq!(said, want);
    assert!(!said.contains("not running"), "{said}");
}

#[tokio::test(start_paused = true)]
async fn a_bare_down_names_the_unreachable_service_and_keeps_code_0() {
    let dir = TempDir::new("down-service-bare");
    let layout = layout(&dir);
    let roles = FakeRoles::unreachable_broker(ServiceState::Answers);
    let code = down_with(&layout, Vec::new(), false, &roles, pending::<()>()).await;
    assert_eq!(code, Ok(0));
    assert!(roles.said().join("\n").contains("admin channel"));
}

#[tokio::test(start_paused = true)]
async fn down_broker_still_says_not_running_when_nothing_answers() {
    let dir = TempDir::new("down-nothing");
    let layout = layout(&dir);
    let roles = FakeRoles::unreachable_broker(ServiceState::NotInstalled);
    let code = down_with(
        &layout,
        vec!["broker".into()],
        false,
        &roles,
        pending::<()>(),
    )
    .await;
    assert_eq!(code, Ok(1));
    assert_eq!(roles.said_err(), vec!["broker: not running".to_string()]);

    let stopped = FakeRoles::unreachable_broker(ServiceState::InstalledNotAnswering);
    let code = down_with(
        &layout,
        vec!["broker".into()],
        false,
        &stopped,
        pending::<()>(),
    )
    .await;
    assert_eq!(code, Ok(1));
    let said = stopped.said_err().join("\n");
    assert!(
        said.contains("installed as an OS service and not running"),
        "{said}"
    );
}

#[tokio::test(start_paused = true)]
async fn up_broker_and_down_broker_agree() {
    let dir = TempDir::new("down-up-agree");
    let layout = layout(&dir);
    let running = FakeRoles::unreachable_broker(ServiceState::Answers);
    down_with(
        &layout,
        vec!["broker".into()],
        false,
        &running,
        pending::<()>(),
    )
    .await
    .unwrap();
    let up = service_broker(true).unwrap();
    assert!(
        running.said_err()[0].starts_with(&format!("{up}; ")),
        "{:?}",
        running.said_err()
    );

    let stopped = FakeRoles::unreachable_broker(ServiceState::InstalledNotAnswering);
    down_with(
        &layout,
        vec!["broker".into()],
        false,
        &stopped,
        pending::<()>(),
    )
    .await
    .unwrap();
    let up = service_broker(false).unwrap_err();
    let up_head = up.split(';').next().unwrap();
    assert!(
        stopped.said_err()[0].starts_with(up_head),
        "{:?}",
        stopped.said_err()
    );
}

#[tokio::test(start_paused = true)]
async fn an_installed_service_whose_state_cannot_be_read_is_not_called_stopped() {
    let dir = TempDir::new("down-unknown");
    let layout = layout(&dir);
    let roles = FakeRoles::unreachable_broker(ServiceState::InstalledStateUnknown);
    let code = down_with(
        &layout,
        vec!["broker".into()],
        false,
        &roles,
        pending::<()>(),
    )
    .await;
    assert_eq!(code, Ok(1));
    let want = format!(
        "broker: installed as an OS service; down cannot tell whether it runs (the service manager did not answer an unprivileged query) and its admin channel is open only to an administrator. Check it from an elevated shell: {}",
        status_command(Os::current())
    );
    assert_eq!(roles.said_err(), vec![want]);

    let bare = FakeRoles::unreachable_broker(ServiceState::InstalledStateUnknown);
    let code = down_with(&layout, Vec::new(), false, &bare, pending::<()>()).await;
    assert_eq!(code, Ok(0));
    assert!(!bare.said().join("\n").contains("not running"));
}

#[test]
fn an_unreadable_service_manager_is_told_apart_from_a_stopped_service() {
    assert_eq!(service_state(true, None, true), ServiceState::Answers);
    assert_eq!(
        service_state(false, Some(true), true),
        ServiceState::Answers
    );
    assert_eq!(
        service_state(false, Some(false), true),
        ServiceState::InstalledNotAnswering
    );
    assert_eq!(
        service_state(false, None, true),
        ServiceState::InstalledStateUnknown
    );
    assert_eq!(
        service_state(false, None, false),
        ServiceState::NotInstalled
    );
    assert_eq!(
        service_state(false, Some(false), false),
        ServiceState::NotInstalled
    );
}
