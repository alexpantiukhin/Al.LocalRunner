mod common;

use common::fake_roles::{with_runner, FakeRoles};
use common::TempDir;
use lr_cli::commands::confirm::Console;
use lr_cli::commands::up_cmd::RunningRoles;
use lr_cli::commands::upgrade_cmd::{
    parse_version, restart_roles, restart_summary, stop_names, verdict, Job, Verdict,
};
use lr_cli::commands::upgrade_jobs::Activity;
use lr_cli::commands::RoleArg;
use lr_cli::os_service::{Action, System};
use lr_proto::DownMode;
use std::path::Path;
use std::sync::Mutex;

fn v(text: &str) -> lr_cli::commands::upgrade_cmd::Version {
    parse_version(text).expect(text)
}

#[test]
fn version_lines_of_the_binary_and_of_the_build_are_read() {
    let a = v("localrunner 0.1.1 (protocol 1.0)");
    assert_eq!(a.number, (0, 1, 1));
    assert_eq!(a.text, "0.1.1");
    assert_eq!(a.protocol_major, Some(1));
    let rc = v("0.2.0-rc1");
    assert_eq!((rc.number, rc.text.as_str()), ((0, 2, 0), "0.2.0-rc1"));
    assert_eq!(rc.protocol_major, None);
    assert!(parse_version("localrunner (protocol 1.0)").is_none());
    assert!(parse_version("").is_none());
}

#[test]
fn the_verdict_separates_same_older_newer_and_a_protocol_change() {
    let old = v("0.1.0 (protocol 1.0)");
    assert_eq!(
        verdict(Some(&old), &v("0.1.1 (protocol 1.0)")),
        Verdict::Upgrade {
            protocol_changed: false
        }
    );
    assert_eq!(
        verdict(Some(&old), &v("0.2.0 (protocol 2.0)")),
        Verdict::Upgrade {
            protocol_changed: true
        }
    );
    assert_eq!(verdict(Some(&old), &old), Verdict::UpToDate);
    assert_eq!(
        verdict(Some(&v("0.1.1 (protocol 1.0)")), &old),
        Verdict::Downgrade
    );
    assert_eq!(
        verdict(
            Some(&v("0.1.1-rc1 (protocol 1.0)")),
            &v("0.1.1 (protocol 1.0)")
        ),
        Verdict::Upgrade {
            protocol_changed: false
        },
        "a release replaces its release candidate"
    );
    assert_eq!(
        verdict(None, &old),
        Verdict::Upgrade {
            protocol_changed: false
        }
    );
}

fn running(hub: bool, broker: bool, runners: &[&str]) -> RunningRoles {
    RunningRoles {
        hub,
        broker,
        runners: runners.iter().map(|r| r.to_string()).collect(),
    }
}

#[test]
fn the_service_broker_is_neither_stopped_nor_started() {
    let all = running(true, true, &["r1", "r2"]);
    assert_eq!(stop_names(&all, false), ["r1", "r2", "hub", "broker"]);
    assert_eq!(stop_names(&all, true), ["r1", "r2", "hub"]);
    assert_eq!(
        restart_roles(&all, false),
        [RoleArg::Broker, RoleArg::Hub, RoleArg::Runner]
    );
    assert_eq!(restart_roles(&all, true), [RoleArg::Hub, RoleArg::Runner]);
    assert!(stop_names(&running(false, false, &[]), false).is_empty());
    assert!(restart_roles(&running(false, false, &[]), false).is_empty());
}

#[test]
fn the_plan_names_the_runners_that_start_again() {
    let all = running(true, false, &["r1", "r2"]);
    assert_eq!(restart_summary(&all, false), "hub, runner (r1, r2)");
    assert_eq!(restart_summary(&running(false, false, &[]), false), "");
}

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

impl System for Recorder {
    fn apply(&self, action: &Action) -> Result<(), String> {
        self.0.lock().unwrap().push(action.to_string());
        Ok(())
    }
}

struct Case {
    old: Option<&'static str>,
    new: &'static str,
    running: RunningRoles,
    activity: Activity,
    yes: bool,
    cancel: bool,
    dry_run: bool,
    terminal: bool,
    typed: &'static str,
    answering: bool,
    interrupted: bool,
}

struct Outcome {
    result: Result<i32, String>,
    copies: Vec<String>,
    told: Vec<(String, DownMode)>,
    started: Vec<Vec<String>>,
    hub_stops: usize,
    broker_stops: usize,
    out: String,
}

async fn run(case: Case) -> Outcome {
    let dir = TempDir::new("upgrade");
    let layout = with_runner(&dir);
    let roles = FakeRoles::new(false, if case.answering { usize::MAX } else { 0 });
    let system = Recorder::default();
    let started = Mutex::new(Vec::new());
    let start = |argv: &[String]| -> std::io::Result<u32> {
        started.lock().unwrap().push(argv.to_vec());
        Ok(1)
    };
    let (old, new) = (case.old.map(v), v(case.new));
    let job = Job {
        layout: &layout,
        dir: Path::new("bin"),
        exe: Path::new("new/localrunner"),
        old: old.as_ref(),
        new: &new,
        running: &case.running,
        activity: &case.activity,
        yes: case.yes,
        cancel: case.cancel,
        dry_run: case.dry_run,
    };
    let mut input = case.typed.as_bytes();
    let mut out = Vec::new();
    let mut console = Console {
        terminal: case.terminal,
        input: &mut input,
        out: &mut out,
    };
    let result = job
        .run(&roles, &system, &start, &mut console, async {
            if !case.interrupted {
                std::future::pending::<()>().await;
            }
        })
        .await;
    let told = roles.told.lock().unwrap().clone();
    let copies = system.0.lock().unwrap().clone();
    let started = started.lock().unwrap().clone();
    let (hub_stops, broker_stops) = (
        roles.hub_stops.load(std::sync::atomic::Ordering::SeqCst),
        roles.broker_stops(),
    );
    let out = String::from_utf8_lossy(&out).into_owned();
    Outcome {
        result,
        copies,
        told,
        started,
        hub_stops,
        broker_stops,
        out,
    }
}

fn case(old: &'static str, new: &'static str) -> Case {
    Case {
        old: Some(old),
        new,
        running: running(false, false, &["r1"]),
        activity: Activity::default(),
        yes: true,
        cancel: false,
        dry_run: false,
        terminal: false,
        typed: "",
        answering: false,
        interrupted: false,
    }
}

#[tokio::test(start_paused = true)]
async fn an_up_to_date_node_and_a_downgrade_change_nothing() {
    let same = run(case("0.1.1 (protocol 1.0)", "0.1.1 (protocol 1.0)")).await;
    assert_eq!(same.result, Ok(0));
    assert!(same.copies.is_empty() && same.told.is_empty());
    let back = run(case("0.1.1 (protocol 1.0)", "0.1.0 (protocol 1.0)")).await;
    assert!(back.result.unwrap_err().contains("older"));
    assert!(back.copies.is_empty() && back.told.is_empty());
}

#[tokio::test(start_paused = true)]
async fn without_yes_and_without_a_terminal_nothing_is_stopped() {
    let mut c = case("0.1.0 (protocol 1.0)", "0.1.1 (protocol 1.0)");
    c.yes = false;
    let o = run(c).await;
    assert!(o.result.unwrap_err().ends_with("confirm with --yes"));
    assert!(o.copies.is_empty() && o.told.is_empty() && o.started.is_empty());
}

#[tokio::test(start_paused = true)]
async fn dry_run_prints_the_copy_and_touches_nothing() {
    let mut c = case("0.1.0 (protocol 1.0)", "0.1.1 (protocol 1.0)");
    c.dry_run = true;
    c.yes = false;
    let o = run(c).await;
    assert_eq!(o.result, Ok(0));
    assert!(o.copies.is_empty() && o.told.is_empty() && o.started.is_empty());
}

#[tokio::test(start_paused = true)]
async fn upgrade_drains_the_runner_replaces_the_binary_and_starts_it_from_the_installed_one() {
    let o = run(case("0.1.0 (protocol 1.0)", "0.1.1 (protocol 1.0)")).await;
    assert_eq!(o.told, vec![("r1".to_string(), DownMode::Drain)]);
    assert_eq!(
        o.copies.len(),
        if cfg!(windows) { 2 } else { 1 },
        "{:?}",
        o.copies
    );
    assert!(o.copies[0].contains("new") && o.copies[0].contains("bin"));
    assert_eq!(o.started.len(), 1, "{:?}", o.started);
    assert_eq!(&o.started[0][..3], ["runner", "run", "--name"]);
}

#[tokio::test(start_paused = true)]
async fn an_idle_node_is_only_replaced() {
    let mut c = case("0.1.0 (protocol 1.0)", "0.1.1 (protocol 1.0)");
    c.running = running(false, false, &[]);
    let o = run(c).await;
    assert_eq!(o.result, Ok(0));
    assert!(o.told.is_empty() && o.started.is_empty());
    assert!(!o.copies.is_empty());
}

fn busy() -> Activity {
    Activity {
        running: vec!["j1 on r1 (tests)".into()],
        queued: 2,
        note: None,
    }
}

fn busy_case() -> Case {
    let mut c = case("0.1.0 (protocol 1.0)", "0.1.1 (protocol 1.0)");
    c.activity = busy();
    c
}

#[test]
fn the_activity_lines_name_running_jobs_and_the_waiting_orders() {
    let lines = busy().lines();
    assert_eq!(lines[0], "running jobs: 1");
    assert!(lines[1].contains("j1 on r1"));
    assert!(lines[2].starts_with("waiting orders: 2"));
    assert_eq!(Activity::default().lines(), ["no job runs or waits"]);
    let unknown = Activity {
        note: Some("the hub is not running".into()),
        ..Activity::default()
    };
    assert!(unknown.lines()[0].contains("hub"));
}

#[tokio::test(start_paused = true)]
async fn running_jobs_without_yes_and_without_a_terminal_stop_nothing() {
    let mut c = busy_case();
    c.yes = false;
    let o = run(c).await;
    let e = o.result.unwrap_err();
    assert!(e.contains("--yes") && e.contains("--cancel"), "{e}");
    assert!(o.told.is_empty() && o.copies.is_empty());
}

#[tokio::test(start_paused = true)]
async fn yes_waits_for_the_running_jobs_and_yes_cancel_cancels_them() {
    let wait = run(busy_case()).await;
    assert_eq!(wait.told, vec![("r1".to_string(), DownMode::Drain)]);
    let mut c = busy_case();
    c.cancel = true;
    let cancel = run(c).await;
    assert_eq!(cancel.told, vec![("r1".to_string(), DownMode::Cancel)]);
}

#[tokio::test(start_paused = true)]
async fn at_a_terminal_the_user_picks_wait_cancel_or_abort() {
    for (typed, told) in [
        ("w\n", Some(DownMode::Drain)),
        ("cancel\n", Some(DownMode::Cancel)),
        ("a\n", None),
        ("\n", None),
    ] {
        let mut c = busy_case();
        c.yes = false;
        c.terminal = true;
        c.typed = typed;
        let o = run(c).await;
        assert_eq!(o.told.first().map(|t| t.1), told, "{typed:?}: {:?}", o.out);
        assert!(o.out.contains("[w]ait"), "{}", o.out);
        if told.is_none() {
            assert_eq!(o.result, Ok(1));
            assert!(o.out.ends_with("aborted\n"), "{}", o.out);
            assert!(o.copies.is_empty());
        }
    }
}

#[tokio::test(start_paused = true)]
async fn ctrl_c_while_waiting_leaves_the_hub_the_broker_and_the_binary_alone() {
    let mut c = busy_case();
    c.running = running(true, true, &["r1"]);
    c.answering = true;
    c.interrupted = true;
    let o = run(c).await;
    assert_eq!(o.result, Ok(1));
    assert_eq!(o.told, vec![("r1".to_string(), DownMode::Drain)]);
    assert_eq!((o.hub_stops, o.broker_stops), (0, 0));
    assert!(o.copies.is_empty() && o.started.is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_hub_and_the_broker_stop_after_the_runners_are_gone() {
    let mut c = busy_case();
    c.running = running(true, true, &["r1"]);
    let o = run(c).await;
    assert_eq!((o.hub_stops, o.broker_stops), (1, 1));
    assert!(!o.copies.is_empty());
}
