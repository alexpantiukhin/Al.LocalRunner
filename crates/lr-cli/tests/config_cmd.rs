mod common;

use common::fake_roles::{with_runner, FakeControl, FakeElevator};
use common::TempDir;
use lr_cli::commands::config_cmd::{edit_with, get, show, Edit, EditArgs};
use lr_cli::commands::down_cmd::Target as Role;
use lr_cli::config_edit::{restart_line, restart_plan, RestartPlan, Target};
use lr_cli::os_service::Os;
use lr_cli::paths::Layout;
use std::path::PathBuf;

struct Run {
    code: Result<i32, String>,
    out: String,
    err: String,
}

fn runner_file(layout: &Layout) -> PathBuf {
    layout.runner_dir("r1").join("runner.toml")
}

fn runner_layout(dir: &TempDir, text: &str) -> Layout {
    let layout = with_runner(dir);
    std::fs::write(runner_file(&layout), text).unwrap();
    layout
}

fn set(target: Target, key: &str, value: &str) -> EditArgs {
    EditArgs {
        target,
        edit: Edit::Set {
            key: key.into(),
            value: value.into(),
            string: false,
        },
        dry_run: false,
        restart: false,
    }
}

fn r1() -> Target {
    Target::Runner("r1".into())
}

async fn run(
    layout: &Layout,
    args: EditArgs,
    child: bool,
    roles: &FakeControl,
    elevator: &FakeElevator,
) -> Run {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = edit_with(layout, args, child, elevator, roles, &mut out, &mut err).await;
    Run {
        code,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

const ENV: &str = "\n[env]\npath = \"/bin\"\n";
const RUNNER: &str = "# my runner\nmem_mb = 1024\ncpu = 1\n\n[env]\npath = \"/bin\"\n";

#[tokio::test]
async fn set_writes_file_and_reloads_running_role() {
    let dir = TempDir::new("config-set");
    let layout = runner_layout(&dir, RUNNER);
    let roles = FakeControl::new(false, true);
    let elevator = FakeElevator::new(Ok(0));
    let r = run(&layout, set(r1(), "cpu", "2"), false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert_eq!(
        std::fs::read_to_string(runner_file(&layout)).unwrap(),
        format!("# my runner\nmem_mb = 1024\ncpu = 2\n{ENV}")
    );
    assert!(
        r.out.contains(&runner_file(&layout).display().to_string()),
        "{}",
        r.out
    );
    assert!(r.out.contains("-cpu = 1\n+cpu = 2"), "{}", r.out);
    assert!(r.out.contains("r1: reloaded"), "{}", r.out);
    assert_eq!(roles.reloads(), [Role::Runner("r1".into())]);
    assert!(roles.restarts().is_empty());
    assert!(elevator.calls().is_empty());
}

#[tokio::test]
async fn dry_run_writes_nothing_and_needs_no_rights() {
    let dir = TempDir::new("config-dry-run");
    let layout = runner_layout(&dir, RUNNER);
    let roles = FakeControl::new(true, true);
    let elevator = FakeElevator::new(Ok(0));
    let mut args = set(r1(), "cpu", "2");
    args.dry_run = true;
    let r = run(&layout, args, false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("+cpu = 2"), "{}", r.out);
    assert!(r.out.contains("dry run"), "{}", r.out);
    assert_eq!(
        std::fs::read_to_string(runner_file(&layout)).unwrap(),
        RUNNER
    );
    assert!(elevator.calls().is_empty());
    assert!(roles.reloads().is_empty());
}

#[tokio::test]
async fn not_running_role_is_reported_and_exit_zero() {
    let dir = TempDir::new("config-not-running");
    let layout = runner_layout(&dir, RUNNER);
    let roles = FakeControl::new(false, false);
    let elevator = FakeElevator::new(Ok(0));
    let r = run(&layout, set(r1(), "cpu", "2"), false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("r1: not running"), "{}", r.out);
    assert!(std::fs::read_to_string(runner_file(&layout))
        .unwrap()
        .contains("cpu = 2"));
}

#[tokio::test]
async fn allow_elevated_prints_widens_line_and_next_start_note() {
    let dir = TempDir::new("config-widens");
    let layout = runner_layout(&dir, RUNNER);
    let roles = FakeControl::new(false, true);
    let elevator = FakeElevator::new(Ok(0));
    let r = run(
        &layout,
        set(r1(), "allow_elevated", "true"),
        false,
        &roles,
        &elevator,
    )
    .await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(
        r.out.contains("widens privileges: allow_elevated"),
        "{}",
        r.out
    );
    assert!(r.out.contains("allow_elevated = true"), "{}", r.out);
    assert!(r.out.contains("applies at the next start"), "{}", r.out);
    assert!(r.out.contains("restart-command-of-r1"), "{}", r.out);
    assert!(roles.restarts().is_empty());
}

#[tokio::test]
async fn restart_only_key_names_the_command_and_does_not_restart_without_flag() {
    let dir = TempDir::new("config-restart-only");
    let layout = with_runner(&dir);
    let policy = layout.config_file("policy.toml");
    std::fs::write(&policy, "[listen]\naddress = []\n").unwrap();
    let roles = FakeControl::new(false, true);
    let elevator = FakeElevator::new(Ok(0));
    let args = || set(Target::Broker, "listen.address", "[\"127.0.0.1:7341\"]");
    let r = run(&layout, args(), false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("broker: reloaded"), "{}", r.out);
    assert!(r.out.contains("applies at the next start"), "{}", r.out);
    assert!(r.out.contains("restart-command-of-broker"), "{}", r.out);
    assert!(r.out.contains("connections drop"), "{}", r.out);
    assert!(roles.restarts().is_empty(), "no restart without --restart");

    let mut again = set(Target::Broker, "listen.address", "[\"127.0.0.1:7342\"]");
    again.restart = true;
    let r = run(&layout, again, false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert_eq!(roles.restarts(), [Role::Broker]);
    assert!(r.out.contains("connections drop"), "{}", r.out);

    let mut other = set(Target::Broker, "queue.strategy", "fair-share");
    other.restart = true;
    let r = run(&layout, other, false, &roles, &elevator).await;
    assert_eq!(roles.restarts(), [Role::Broker], "{} {}", r.out, r.err);
}

#[tokio::test]
async fn config_get_and_show() {
    let dir = TempDir::new("config-get");
    let layout = runner_layout(&dir, RUNNER);
    assert_eq!(get(&layout, &r1(), "cpu"), Ok("1".to_string()));
    let missing = get(&layout, &r1(), "tags").unwrap_err();
    assert!(missing.contains("not set"), "{missing}");
    let shown = show(&layout, &r1()).unwrap();
    assert!(
        shown.contains(&runner_file(&layout).display().to_string()),
        "{shown}"
    );
    assert!(shown.contains(RUNNER), "{shown}");
    let hub = show(&layout, &Target::Hub).unwrap();
    assert!(hub.contains("does not exist"), "{hub}");
}

#[tokio::test]
async fn unset_missing_key_exit_nonzero() {
    let dir = TempDir::new("config-unset");
    let layout = runner_layout(&dir, RUNNER);
    let roles = FakeControl::new(false, true);
    let elevator = FakeElevator::new(Ok(0));
    let unset = |key: &str| EditArgs {
        target: r1(),
        edit: Edit::Unset { key: key.into() },
        dry_run: false,
        restart: false,
    };
    let r = run(&layout, unset("tags"), false, &roles, &elevator).await;
    assert!(
        r.code.as_ref().unwrap_err().contains("not set"),
        "{:?}",
        r.code
    );
    assert_eq!(
        std::fs::read_to_string(runner_file(&layout)).unwrap(),
        RUNNER
    );

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_localrunner"))
        .arg("--state-dir")
        .arg(&layout.root)
        .arg("--config-dir")
        .arg(&layout.config)
        .args(["config", "unset", "hub", "brokers.strategy"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not set"),
        "{out:?}"
    );
    assert!(!layout.config_file("hub-policy.toml").exists());

    let r = run(&layout, unset("cpu"), false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert_eq!(
        std::fs::read_to_string(runner_file(&layout)).unwrap(),
        format!("# my runner\nmem_mb = 1024\n{ENV}")
    );
}

#[tokio::test]
async fn needs_rights_goes_through_the_elevator() {
    let dir = TempDir::new("config-elevate");
    let layout = runner_layout(&dir, RUNNER);
    let roles = FakeControl::new(true, true);
    let elevator = FakeElevator::new(Ok(0));
    let mut args = set(r1(), "cpu", "2");
    args.restart = true;
    let r = run(&layout, args, false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(
        r.out.contains("+cpu = 2"),
        "the diff is shown before elevating: {}",
        r.out
    );
    assert_eq!(
        std::fs::read_to_string(runner_file(&layout)).unwrap(),
        RUNNER
    );
    assert_eq!(
        roles.reloads(),
        [Role::Runner("r1".into())],
        "the parent reloads the user roles after the child exits 0"
    );
    assert_eq!(roles.restarts(), Vec::<Role>::new());
    let calls = elevator.calls();
    assert_eq!(calls.len(), 1);
    let root = layout.root.display().to_string();
    let config = layout.config.display().to_string();
    assert_eq!(
        calls[0].1,
        [
            "--state-dir",
            root.as_str(),
            "--config-dir",
            config.as_str(),
            "config",
            "set",
            "runner:r1",
            "cpu",
            "2",
            "--restart"
        ]
    );

    let child = run(&layout, set(r1(), "cpu", "2"), true, &roles, &elevator).await;
    assert_eq!(child.code, Ok(0), "{} {}", child.out, child.err);
    assert!(
        !child.out.contains("@@"),
        "the child does not show the diff again: {}",
        child.out
    );
    assert_eq!(elevator.calls().len(), 1, "the child never elevates");
    assert!(std::fs::read_to_string(runner_file(&layout))
        .unwrap()
        .contains("cpu = 2"));
    assert_eq!(roles.reloads(), [Role::Runner("r1".into())]);

    let refused = FakeElevator::new(Err("the administrator prompt was refused".into()));
    let r = run(&layout, set(r1(), "cpu", "1"), false, &roles, &refused).await;
    assert_eq!(r.code, Ok(1), "{} {}", r.out, r.err);
    assert!(r.err.contains("refused"), "{}", r.err);
    assert!(std::fs::read_to_string(runner_file(&layout))
        .unwrap()
        .contains("cpu = 2"));
}

#[tokio::test]
async fn the_elevated_child_handles_only_the_broker_and_the_parent_the_rest() {
    let dir = TempDir::new("config-phases");
    let layout = runner_layout(&dir, RUNNER);
    std::fs::write(
        layout.config_file("policy.toml"),
        "[listen]\naddress = []\n",
    )
    .unwrap();
    let elevator = FakeElevator::new(Ok(0));
    let child = FakeControl::new(true, true);
    let r = run(&layout, set(r1(), "cpu", "2"), true, &child, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(
        child.reloads().is_empty(),
        "a runner is reloaded by the parent"
    );

    let mut listen = set(Target::Broker, "listen.address", "[\"127.0.0.1:7341\"]");
    listen.restart = true;
    let r = run(&layout, listen.clone(), true, &child, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert_eq!(child.reloads(), [Role::Broker]);
    assert_eq!(child.restarts(), [Role::Broker]);

    let parent = FakeControl::new(true, true);
    let mut listen = set(Target::Broker, "listen.address", "[\"127.0.0.1:7342\"]");
    listen.restart = true;
    let r = run(&layout, listen, false, &parent, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(parent.reloads().is_empty(), "the child reloaded the broker");
    assert!(
        parent.restarts().is_empty(),
        "the child restarted the broker"
    );
}

#[tokio::test]
async fn a_failed_child_exit_code_is_the_commands_and_nothing_follows() {
    let dir = TempDir::new("config-child-fails");
    let layout = runner_layout(&dir, RUNNER);
    let roles = FakeControl::new(true, true);
    let elevator = FakeElevator::new(Ok(3));
    let mut args = set(r1(), "allow_elevated", "true");
    args.restart = true;
    let r = run(&layout, args, false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(3), "{} {}", r.out, r.err);
    assert!(roles.reloads().is_empty());
    assert!(roles.restarts().is_empty());
    assert!(!r.out.contains("applies at the next start"), "{}", r.out);
}

#[tokio::test]
async fn a_reload_error_after_the_write_still_names_the_restart() {
    let dir = TempDir::new("config-reload-error");
    let layout = runner_layout(&dir, RUNNER);
    let mut roles = FakeControl::new(false, true);
    roles.reload_error = Some("no principal".into());
    let elevator = FakeElevator::new(Ok(0));
    let r = run(
        &layout,
        set(r1(), "allow_elevated", "true"),
        false,
        &roles,
        &elevator,
    )
    .await;
    assert_eq!(r.code, Ok(1), "{} {}", r.out, r.err);
    assert!(r.out.contains("written:"), "{}", r.out);
    assert!(
        r.err.contains("r1: cannot reload: no principal"),
        "{}",
        r.err
    );
    assert!(r.out.contains("applies at the next start"), "{}", r.out);
    assert!(r.out.contains("restart-command-of-r1"), "{}", r.out);
    assert!(std::fs::read_to_string(runner_file(&layout))
        .unwrap()
        .contains("allow_elevated = true"));
}

#[tokio::test]
async fn a_refused_reload_keeps_the_file_and_exits_one() {
    let dir = TempDir::new("config-refused");
    let layout = runner_layout(&dir, RUNNER);
    let mut roles = FakeControl::new(false, true);
    roles.refuse = Some("runner.toml: bad cpu".into());
    let elevator = FakeElevator::new(Ok(0));
    let r = run(&layout, set(r1(), "cpu", "2"), false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(1), "{} {}", r.out, r.err);
    assert!(r.err.contains("r1: runner.toml: bad cpu"), "{}", r.err);
    assert!(std::fs::read_to_string(runner_file(&layout))
        .unwrap()
        .contains("cpu = 2"));
}

#[tokio::test]
async fn a_repeated_command_reloads_and_restarts_again() {
    let dir = TempDir::new("config-repeat");
    let layout = runner_layout(&dir, RUNNER);
    std::fs::write(
        layout.config_file("policy.toml"),
        "[listen]\naddress = [\"127.0.0.1:7341\"]\n",
    )
    .unwrap();
    let roles = FakeControl::new(false, true);
    let elevator = FakeElevator::new(Ok(0));
    let r = run(&layout, set(r1(), "cpu", "1"), false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("no change in"), "{}", r.out);
    assert!(!r.out.contains("written:"), "{}", r.out);
    assert_eq!(roles.reloads(), [Role::Runner("r1".into())]);
    assert_eq!(
        std::fs::read_to_string(runner_file(&layout)).unwrap(),
        RUNNER
    );

    let mut again = set(Target::Broker, "listen.address", "[\"127.0.0.1:7341\"]");
    again.restart = true;
    let r = run(&layout, again, false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("no change in"), "{}", r.out);
    assert_eq!(roles.restarts(), [Role::Broker]);
}

#[test]
fn a_runner_restart_stops_and_starts_only_that_runner() {
    let dir = TempDir::new("config-restart-plan");
    let layout = with_runner(&dir);
    std::fs::create_dir_all(layout.root.join("runner").join("r2")).unwrap();
    let role = Role::Runner("my box".into());
    let plan = restart_plan(&role, None);
    assert_eq!(
        plan,
        RestartPlan::Process {
            stop: vec!["my box".into()],
            start: ["runner", "run", "--name", "my box", "--background"]
                .map(String::from)
                .to_vec(),
        }
    );
    let line = restart_line(Os::Linux, &layout, &plan);
    let root = layout.root.display().to_string();
    assert!(line.contains("down 'my box' --wait"), "{line}");
    assert!(
        line.contains("runner run --name 'my box' --background"),
        "{line}"
    );
    assert!(line.contains("--state-dir"), "{line}");
    assert!(
        line.contains(&root) || line.contains(&format!("'{root}'")),
        "{line}"
    );
    assert!(
        !line.contains("r2") && !line.contains("up runner"),
        "{line}"
    );
    let windows = restart_line(Os::Windows, &layout, &plan);
    assert!(windows.contains("--name 'my box'"), "{windows}");
    let service = restart_plan(&role, Some("Restart-Service AlLocalRunner-x".into()));
    assert_eq!(
        restart_line(Os::Windows, &layout, &service),
        "Restart-Service AlLocalRunner-x"
    );
}
