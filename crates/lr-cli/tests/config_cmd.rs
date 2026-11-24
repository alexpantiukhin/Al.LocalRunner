mod common;

use common::fake_roles::{with_runner, FakeControl, FakeElevator, FakeProcesses, FakeRoles};
use common::TempDir;
use lr_cli::commands::config_cmd::{edit_with, get, show, Edit, EditArgs};
use lr_cli::commands::down_cmd::stop_one_with;
use lr_cli::commands::down_cmd::Target as Role;
use lr_cli::config_edit::{
    restart_line, restart_plan, restart_process, restart_service, RestartOutcome, RestartPlan,
    Target, EXIT_FILE_CHANGED, EXIT_WRITTEN_STEP_FAILED,
};
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
        expected_before: None,
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
    assert!(r.out.contains("r1: reloaded"), "{}", r.out);
    assert!(r.out.contains("applies at the next start"), "{}", r.out);
    assert!(r.out.contains("restart-command-of-r1"), "{}", r.out);
    assert!(r.out.contains("connections drop"), "{}", r.out);
    assert!(roles.restarts().is_empty(), "no restart without --restart");

    let mut again = set(r1(), "allow_elevated", "false");
    again.restart = true;
    let r = run(&layout, again, false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert_eq!(roles.restarts(), [Role::Runner("r1".into())]);
    assert!(r.out.contains("connections drop"), "{}", r.out);

    let mut other = set(r1(), "cpu", "2");
    other.restart = true;
    let r = run(&layout, other, false, &roles, &elevator).await;
    assert_eq!(
        roles.restarts(),
        [Role::Runner("r1".into())],
        "{} {}",
        r.out,
        r.err
    );
}

#[tokio::test]
async fn listen_applies_on_reload_without_a_restart() {
    let dir = TempDir::new("config-listen-live");
    let layout = with_runner(&dir);
    std::fs::write(
        layout.config_file("policy.toml"),
        "[listen]\naddress = []\n",
    )
    .unwrap();
    let roles = FakeControl::new(false, true);
    let elevator = FakeElevator::new(Ok(0));
    let mut listen = set(Target::Broker, "listen.address", "[\"127.0.0.1:7341\"]");
    listen.restart = true;
    let r = run(&layout, listen, false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("broker: reloaded"), "{}", r.out);
    assert!(!r.out.contains("applies at the next start"), "{}", r.out);
    assert!(roles.restarts().is_empty(), "{}", r.out);
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
        expected_before: None,
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
    let expect = format!(
        "--expect-before={}",
        lr_cli::runner_reload::hash_text(RUNNER)
    );
    assert_eq!(
        calls[0].1,
        [
            expect.as_str(),
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

    let mut child = child;
    child.restart_required = vec!["a broker setting".into()];
    let mut listen = set(Target::Broker, "listen.address", "[\"127.0.0.1:7341\"]");
    listen.restart = true;
    let r = run(&layout, listen.clone(), true, &child, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert_eq!(child.reloads(), [Role::Broker]);
    assert_eq!(child.restarts(), [Role::Broker]);

    let mut parent = FakeControl::new(true, true);
    parent.restart_required = vec!["a broker setting".into()];
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
    let layout = runner_layout(
        &dir,
        &format!("allow_elevated = true\nmem_mb = 1024\ncpu = 1\n{ENV}"),
    );
    let roles = FakeControl::new(false, true);
    let elevator = FakeElevator::new(Ok(0));
    let r = run(&layout, set(r1(), "cpu", "1"), false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("no change in"), "{}", r.out);
    assert!(!r.out.contains("written:"), "{}", r.out);
    assert_eq!(roles.reloads(), [Role::Runner("r1".into())]);
    assert_eq!(
        std::fs::read_to_string(runner_file(&layout)).unwrap(),
        format!("allow_elevated = true\nmem_mb = 1024\ncpu = 1\n{ENV}")
    );

    let mut again = set(r1(), "allow_elevated", "true");
    again.restart = true;
    let r = run(&layout, again, false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("no change in"), "{}", r.out);
    assert_eq!(roles.restarts(), [Role::Runner("r1".into())]);
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
            stop: Role::Runner("my box".into()),
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

#[tokio::test]
async fn a_child_that_wrote_but_failed_after_still_lets_the_parent_reload() {
    let dir = TempDir::new("config-child-partial");
    let layout = runner_layout(&dir, RUNNER);
    let roles = FakeControl::new(true, true);
    let partial = FakeElevator::new(Ok(EXIT_WRITTEN_STEP_FAILED));
    let r = run(
        &layout,
        set(Target::Logging, "logging.level", "\"debug\""),
        false,
        &roles,
        &partial,
    )
    .await;
    assert_eq!(r.code, Ok(1), "{} {}", r.out, r.err);
    assert_eq!(
        roles.reloads(),
        [Role::Runner("r1".into()), Role::Hub],
        "the user roles are reloaded after a written file"
    );

    let roles = FakeControl::new(true, true);
    let nothing = FakeElevator::new(Ok(1));
    let r = run(
        &layout,
        set(Target::Logging, "logging.level", "\"debug\""),
        false,
        &roles,
        &nothing,
    )
    .await;
    assert_eq!(r.code, Ok(1), "{} {}", r.out, r.err);
    assert!(roles.reloads().is_empty(), "nothing was written");
}

#[tokio::test]
async fn the_child_says_written_but_failed_when_the_broker_refuses() {
    let dir = TempDir::new("config-child-refused");
    let layout = runner_layout(&dir, RUNNER);
    std::fs::write(
        layout.config_file("policy.toml"),
        "[listen]\naddress = []\n",
    )
    .unwrap();
    let mut roles = FakeControl::new(true, true);
    roles.refuse = Some("policy.toml: no".into());
    let elevator = FakeElevator::new(Ok(0));
    let r = run(
        &layout,
        set(Target::Logging, "logging.level", "\"debug\""),
        true,
        &roles,
        &elevator,
    )
    .await;
    assert_eq!(r.code, Ok(EXIT_WRITTEN_STEP_FAILED), "{} {}", r.out, r.err);
    assert_eq!(roles.reloads(), [Role::Broker]);
    assert!(std::fs::read_to_string(layout.config_file("logging.toml"))
        .unwrap()
        .contains("debug"));

    let roles = FakeControl::new(true, true);
    let r = run(
        &layout,
        set(Target::Logging, "logging.level", "info"),
        true,
        &roles,
        &elevator,
    )
    .await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
}

fn start_args(name: &str) -> Vec<String> {
    ["runner", "run", "--name", name, "--background"]
        .map(String::from)
        .to_vec()
}

#[tokio::test]
async fn a_restart_refused_by_ups_check_stops_nothing() {
    let dir = TempDir::new("config-restart-refused");
    let layout = with_runner(&dir);
    let procs = FakeProcesses::new(Err("the runner refuses to start: same user".into()), true);
    let mut err = Vec::new();
    let role = Role::Runner("r1".into());
    let outcome = restart_process(
        &procs,
        &layout,
        &role,
        &start_args("r1"),
        std::time::Duration::from_secs(15),
        &mut err,
    )
    .await;
    assert_eq!(
        outcome,
        RestartOutcome::Refused("the runner refuses to start: same user".into())
    );
    assert!(procs.stopped.lock().unwrap().is_empty());
    assert!(procs.started.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_restarted_role_that_never_answers_is_not_reported_restarted() {
    let dir = TempDir::new("config-restart-silent");
    let layout = with_runner(&dir);
    let procs = FakeProcesses::new(Ok(Some("same user".into())), false);
    let mut err = Vec::new();
    let role = Role::Runner("runner".into());
    let outcome = restart_process(
        &procs,
        &layout,
        &role,
        &start_args("runner"),
        std::time::Duration::from_secs(15),
        &mut err,
    )
    .await;
    assert_eq!(outcome, RestartOutcome::NotAnswering);
    assert_eq!(
        *procs.stopped.lock().unwrap(),
        [Role::Runner("runner".into())]
    );
    assert_eq!(*procs.started.lock().unwrap(), [start_args("runner")]);
    assert!(String::from_utf8(err)
        .unwrap()
        .contains("warning: same user"));
    assert!(procs.probes.load(std::sync::atomic::Ordering::SeqCst) > 1);

    let answering = FakeProcesses::new(Ok(None), true);
    let outcome = restart_process(
        &answering,
        &layout,
        &role,
        &start_args("runner"),
        std::time::Duration::from_secs(15),
        &mut Vec::new(),
    )
    .await;
    assert_eq!(outcome, RestartOutcome::Restarted);
}

#[tokio::test]
async fn a_runner_named_like_a_role_is_stopped_alone() {
    let wait = std::time::Duration::from_secs(1);
    for name in ["runner", "hub", "broker"] {
        let roles = FakeRoles::new(false, 0);
        let code = stop_one_with(Role::Runner(name.into()), wait, &roles).await;
        assert_eq!(code, Ok(0), "{name}");
        let told: Vec<String> = roles
            .told
            .lock()
            .unwrap()
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        assert_eq!(told, [name], "{name}");
        assert_eq!(roles.hub_stops.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(roles.broker_stops(), 0);
    }
    let roles = FakeRoles::new(false, 0);
    assert_eq!(stop_one_with(Role::Hub, wait, &roles).await, Ok(0));
    assert!(roles.told.lock().unwrap().is_empty());
    assert_eq!(roles.hub_stops.load(std::sync::atomic::Ordering::SeqCst), 1);
    let roles = FakeRoles::new(false, 0);
    assert_eq!(stop_one_with(Role::Broker, wait, &roles).await, Ok(0));
    assert_eq!(roles.broker_stops(), 1);
    assert!(roles.told.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_restarted_service_that_never_answers_is_not_reported_restarted() {
    let dir = TempDir::new("config-service-silent");
    let layout = with_runner(&dir);
    let within = std::time::Duration::from_secs(15);
    for role in [Role::Broker, Role::Runner("r1".into())] {
        let silent = FakeProcesses::new(Ok(None), false);
        let outcome = restart_service(&silent, &layout, &role, "Restart-Service X", within).await;
        assert_eq!(outcome, RestartOutcome::ServiceNotAnswering, "{role:?}");
        assert_eq!(
            *silent.service_restarts.lock().unwrap(),
            ["Restart-Service X"]
        );
        assert!(silent.probes.load(std::sync::atomic::Ordering::SeqCst) > 1);

        let answering = FakeProcesses::new(Ok(None), true);
        let outcome =
            restart_service(&answering, &layout, &role, "Restart-Service X", within).await;
        assert_eq!(outcome, RestartOutcome::Restarted, "{role:?}");
    }
}

fn template_file(layout: &Layout) -> PathBuf {
    layout.config_file("runner.toml")
}

fn read(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[tokio::test]
async fn hub_edit_writes_hub_policy_and_reloads_the_hub() {
    let dir = TempDir::new("config-hub");
    let layout = with_runner(&dir);
    let file = layout.config_file("hub-policy.toml");
    std::fs::write(&file, "[brokers]\nquery_timeout_s = 3\n").unwrap();
    let roles = FakeControl::new(false, true);
    let elevator = FakeElevator::new(Ok(0));
    let r = run(
        &layout,
        set(Target::Hub, "brokers.query_timeout_s", "5"),
        false,
        &roles,
        &elevator,
    )
    .await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert_eq!(read(&file), "[brokers]\nquery_timeout_s = 5\n");
    assert!(r.out.contains(&file.display().to_string()), "{}", r.out);
    assert!(r.out.contains("hub: reloaded"), "{}", r.out);
    assert_eq!(roles.reloads(), [Role::Hub]);
    assert!(!r.out.contains("applies at the next start"), "{}", r.out);
}

#[tokio::test]
async fn a_runner_with_an_own_copy_edits_it_and_says_so() {
    let dir = TempDir::new("config-own-copy");
    let layout = runner_layout(&dir, RUNNER);
    std::fs::write(template_file(&layout), RUNNER).unwrap();
    let roles = FakeControl::new(false, true);
    let elevator = FakeElevator::new(Ok(0));
    let r = run(&layout, set(r1(), "cpu", "4"), false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(read(&runner_file(&layout)).contains("cpu = 4"));
    assert_eq!(read(&template_file(&layout)), RUNNER);
    assert!(
        r.out
            .contains("runner r1: its own copy; edits of the template do not reach it"),
        "{}",
        r.out
    );
    assert!(!r.out.contains("edits the template"), "{}", r.out);
}

#[tokio::test]
async fn a_runner_without_an_own_copy_edits_the_template_and_says_so() {
    let dir = TempDir::new("config-template");
    let layout = with_runner(&dir);
    std::fs::write(template_file(&layout), RUNNER).unwrap();
    assert!(!runner_file(&layout).exists());
    let roles = FakeControl::new(false, true);
    let elevator = FakeElevator::new(Ok(0));
    let r = run(&layout, set(r1(), "cpu", "4"), false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(read(&template_file(&layout)).contains("cpu = 4"));
    assert!(!runner_file(&layout).exists());
    assert!(
        r.out
            .contains("edits the template: every runner without an own copy reads it"),
        "{}",
        r.out
    );
    assert!(!r.out.contains("its own copy"), "{}", r.out);
    assert_eq!(roles.reloads(), [Role::Runner("r1".into())]);

    let unset = EditArgs {
        target: r1(),
        edit: Edit::Unset { key: "cpu".into() },
        dry_run: false,
        restart: false,
        expected_before: None,
    };
    let r = run(&layout, unset, false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("edits the template"), "{}", r.out);
    assert!(!read(&template_file(&layout)).contains("cpu"));
}

#[tokio::test]
async fn logging_edit_writes_logging_toml_and_reloads_every_role() {
    let dir = TempDir::new("config-logging");
    let layout = with_runner(&dir);
    let file = layout.config_file("logging.toml");
    std::fs::write(&file, "[logging]\nlevel = \"info\"\n").unwrap();
    let roles = FakeControl::new(false, true);
    let elevator = FakeElevator::new(Ok(0));
    let r = run(
        &layout,
        set(Target::Logging, "logging.level", "debug"),
        false,
        &roles,
        &elevator,
    )
    .await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert_eq!(read(&file), "[logging]\nlevel = \"debug\"\n");
    let reloads = roles.reloads();
    for role in [Role::Runner("r1".into()), Role::Hub, Role::Broker] {
        assert!(reloads.contains(&role), "{role:?} in {reloads:?}");
    }
    assert_eq!(reloads.len(), 3, "{reloads:?}");
    assert!(r.out.contains("r1: reloaded"), "{}", r.out);
    assert!(r.out.contains("hub: reloaded"), "{}", r.out);
    assert!(r.out.contains("broker: reloaded"), "{}", r.out);
    assert!(roles.restarts().is_empty());
}

#[tokio::test]
async fn a_template_edit_reloads_every_runner_that_reads_the_template() {
    let dir = TempDir::new("config-template-all");
    let layout = with_runner(&dir);
    std::fs::write(template_file(&layout), RUNNER).unwrap();
    std::fs::create_dir_all(layout.runner_dir("r2")).unwrap();
    std::fs::create_dir_all(layout.runner_dir("r3")).unwrap();
    std::fs::write(layout.runner_dir("r3").join("runner.toml"), RUNNER).unwrap();
    let roles = FakeControl::new(false, true);
    let elevator = FakeElevator::new(Ok(0));
    let r = run(&layout, set(r1(), "cpu", "4"), false, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(read(&template_file(&layout)).contains("cpu = 4"));
    assert_eq!(
        roles.reloads(),
        [Role::Runner("r1".into()), Role::Runner("r2".into())],
        "every runner without an own copy reads the template; r3 has its own"
    );
    assert!(r.out.contains("r1: reloaded"), "{}", r.out);
    assert!(r.out.contains("r2: reloaded"), "{}", r.out);
    assert!(!r.out.contains("r3"), "{}", r.out);

    let mut restart = FakeControl::new(false, true);
    restart.restart_required = vec!["services".into()];
    let r = run(&layout, set(r1(), "cpu", "2"), false, &restart, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("restart r1 to apply it"), "{}", r.out);
    assert!(r.out.contains("restart r2 to apply it"), "{}", r.out);
}

#[tokio::test]
async fn logging_edit_skips_a_service_broker_that_reads_its_own_file() {
    let dir = TempDir::new("config-logging-service");
    let layout = with_runner(&dir);
    let file = layout.config_file("logging.toml");
    std::fs::write(&file, "[logging]\nlevel = \"info\"\n").unwrap();
    let mut roles = FakeControl::new(false, true);
    roles.broker_service = true;
    let elevator = FakeElevator::new(Ok(0));
    let r = run(
        &layout,
        set(Target::Logging, "logging.level", "debug"),
        false,
        &roles,
        &elevator,
    )
    .await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert_eq!(roles.reloads(), [Role::Runner("r1".into()), Role::Hub]);
    assert!(
        r.out
            .contains("broker: not reloaded: it runs as the OS service and reads"),
        "{}",
        r.out
    );
    assert!(!r.out.contains("broker: reloaded"), "{}", r.out);
}

#[tokio::test]
async fn the_elevated_child_refuses_a_file_that_changed_after_the_diff() {
    let dir = TempDir::new("config-child-changed");
    let layout = runner_layout(&dir, RUNNER);
    let roles = FakeControl::new(true, true);
    let elevator = FakeElevator::new(Ok(0));
    let mut args = set(r1(), "cpu", "2");
    args.expected_before = Some(lr_cli::runner_reload::hash_text("cpu = 9\n"));
    let r = run(&layout, args, true, &roles, &elevator).await;
    assert_eq!(r.code, Ok(EXIT_FILE_CHANGED), "{} {}", r.out, r.err);
    assert!(
        r.err
            .contains("changed after its diff was shown: nothing written"),
        "{}",
        r.err
    );
    assert_eq!(read(&runner_file(&layout)), RUNNER);
    assert!(roles.reloads().is_empty());
    assert!(elevator.calls().is_empty());

    let mut args = set(r1(), "cpu", "2");
    args.expected_before = Some(lr_cli::runner_reload::hash_text(RUNNER));
    let r = run(&layout, args, true, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(read(&runner_file(&layout)).contains("cpu = 2"));

    let parent = FakeElevator::new(Ok(EXIT_FILE_CHANGED));
    let r = run(&layout, set(r1(), "cpu", "3"), false, &roles, &parent).await;
    assert_eq!(r.code, Ok(EXIT_FILE_CHANGED), "{} {}", r.out, r.err);
}

#[tokio::test]
async fn a_typo_key_in_runner_toml_is_refused_and_nothing_changes() {
    let dir = TempDir::new("config-typo");
    let layout = runner_layout(&dir, RUNNER);
    let roles = FakeControl::new(false, true);
    let elevator = FakeElevator::new(Ok(0));
    let r = run(&layout, set(r1(), "cpuu", "2"), false, &roles, &elevator).await;
    assert!(r.code.is_err(), "{:?} {} {}", r.code, r.out, r.err);
    assert_eq!(read(&runner_file(&layout)), RUNNER);
    assert!(roles.reloads().is_empty());
    assert!(elevator.calls().is_empty());
}

#[tokio::test]
async fn allow_same_user_as_broker_prints_the_widens_line() {
    let dir = TempDir::new("config-widens-same-user");
    let layout = runner_layout(&dir, RUNNER);
    let roles = FakeControl::new(false, true);
    let elevator = FakeElevator::new(Ok(0));
    let r = run(
        &layout,
        set(r1(), "allow_same_user_as_broker", "true"),
        false,
        &roles,
        &elevator,
    )
    .await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(
        r.out
            .contains("widens privileges: allow_same_user_as_broker"),
        "{}",
        r.out
    );
    let off = run(
        &layout,
        set(r1(), "allow_same_user_as_broker", "false"),
        false,
        &roles,
        &elevator,
    )
    .await;
    assert!(!off.out.contains("widens privileges"), "{}", off.out);
}
