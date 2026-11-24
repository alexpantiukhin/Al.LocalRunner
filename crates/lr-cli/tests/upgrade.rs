mod common;

use common::fake_roles::{with_runner, FakeRoles};
use common::TempDir;
use lr_cli::commands::confirm::Console;
use lr_cli::commands::up_cmd::RunningRoles;
use lr_cli::commands::upgrade_cmd::RECONNECT_WITHIN;
use lr_cli::commands::upgrade_cmd::{
    parse_duration, parse_version, restart_roles, restart_summary, stop_names, verdict, Job,
    Verdict,
};
use lr_cli::commands::upgrade_jobs::{pages, Activity, RunningJob, WaitProgress};
use lr_cli::commands::upgrade_pool::{plan_lines, report_lines, Kind, PoolRunner, Reconnected};
use lr_cli::commands::RoleArg;
use lr_cli::os_service::{Action, System};
use lr_proto::{DownMode, DrainReason};
use std::path::{Path, PathBuf};
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
struct Recorder {
    applied: Mutex<Vec<String>>,
    fail: bool,
}

impl System for Recorder {
    fn apply(&self, action: &Action) -> Result<(), String> {
        self.applied.lock().unwrap().push(action.to_string());
        match self.fail {
            true => Err("the file is busy".into()),
            false => Ok(()),
        }
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
    answers: usize,
    interrupted: bool,
    wait_timeout: Option<std::time::Duration>,
    copy_fails: bool,
    pool: Vec<PoolRunner>,
    after_restart: Vec<PoolRunner>,
    service_runner: Option<Option<bool>>,
    pool_hangs: bool,
}

struct Outcome {
    result: Result<i32, String>,
    copies: Vec<String>,
    told: Vec<(String, DownMode)>,
    reasons: Vec<Option<DrainReason>>,
    lock_seen: Vec<bool>,
    lock_left: bool,
    started: Vec<Vec<String>>,
    hub_stops: usize,
    broker_stops: usize,
    out: String,
    said: Vec<String>,
}

async fn run(case: Case) -> Outcome {
    let dir = TempDir::new("upgrade");
    let layout = with_runner(&dir);
    let mut roles = FakeRoles::new(false, case.answers);
    roles.service_runner = case.service_runner;
    roles.pool_hangs = case.pool_hangs;
    let lock = lr_cli::upgrade_lock::lock_path(&layout);
    *roles.lock_file.lock().unwrap() = Some(lock.clone());
    *roles.after_restart.lock().unwrap() = case.after_restart.clone();
    let system = Recorder {
        fail: case.copy_fails,
        ..Recorder::default()
    };
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
        pool: &case.pool,
        yes: case.yes,
        cancel: case.cancel,
        dry_run: case.dry_run,
        wait_timeout: case.wait_timeout,
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
    let reasons = roles.reasons.lock().unwrap().clone();
    let lock_seen = roles.lock_seen.lock().unwrap().clone();
    let copies = system.applied.lock().unwrap().clone();
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
        reasons,
        lock_seen,
        lock_left: lock.exists(),
        started,
        hub_stops,
        broker_stops,
        out,
        said: roles.said(),
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
        answers: 0,
        interrupted: false,
        wait_timeout: None,
        copy_fails: false,
        pool: Vec::new(),
        after_restart: Vec::new(),
        service_runner: None,
        pool_hangs: false,
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
        running_jobs: vec![RunningJob {
            job: "j1".into(),
            runner: Some("local:r1".into()),
            running_ms: Some(0),
        }],
        queued: vec!["j2".into(), "j3".into()],
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
    c.answers = usize::MAX;
    c.interrupted = true;
    let o = run(c).await;
    assert_eq!(o.result, Ok(1));
    assert!(!o.told.is_empty());
    assert!(o
        .told
        .iter()
        .all(|t| *t == ("r1".to_string(), DownMode::Drain)));
    assert_eq!((o.hub_stops, o.broker_stops), (0, 0));
    assert!(o.copies.is_empty());
    assert_eq!(
        o.started.len(),
        1,
        "runners that left come back: {:?}",
        o.started
    );
}

#[tokio::test(start_paused = true)]
async fn the_hub_and_the_broker_stop_after_the_runners_are_gone() {
    let mut c = busy_case();
    c.running = running(true, true, &["r1"]);
    let o = run(c).await;
    assert_eq!((o.hub_stops, o.broker_stops), (1, 1));
    assert!(!o.copies.is_empty());
}

#[tokio::test(start_paused = true)]
async fn upgrade_writes_the_lock_before_stopping_and_removes_it_after() {
    let o = run(case("0.1.0 (protocol 1.0)", "0.1.1 (protocol 1.0)")).await;
    assert!(o.result.is_ok(), "{:?}", o.result);
    assert!(!o.lock_seen.is_empty() && o.lock_seen.iter().all(|seen| *seen));
    assert!(!o.lock_left);
}

#[tokio::test(start_paused = true)]
async fn the_lock_is_removed_after_a_failed_replacement() {
    let mut c = case("0.1.0 (protocol 1.0)", "0.1.1 (protocol 1.0)");
    c.copy_fails = true;
    let o = run(c).await;
    assert!(o.result.is_err());
    assert!(!o.lock_left);
}

#[tokio::test(start_paused = true)]
async fn the_lock_is_removed_when_the_wait_is_interrupted() {
    let mut c = busy_case();
    c.answers = usize::MAX;
    c.interrupted = true;
    let o = run(c).await;
    assert_eq!(o.result, Ok(1));
    assert!(o.lock_seen.iter().all(|seen| *seen));
    assert!(!o.lock_left);
}

#[tokio::test(start_paused = true)]
async fn no_lock_is_written_for_a_dry_run_or_an_up_to_date_node() {
    let mut dry = case("0.1.0 (protocol 1.0)", "0.1.1 (protocol 1.0)");
    dry.dry_run = true;
    assert!(run(dry).await.lock_seen.is_empty());
    let same = run(case("0.1.1 (protocol 1.0)", "0.1.1 (protocol 1.0)")).await;
    assert!(same.lock_seen.is_empty() && !same.lock_left);
}

#[tokio::test(start_paused = true)]
async fn runners_are_stopped_with_the_upgrade_reason() {
    let o = run(case("0.1.0 (protocol 1.0)", "0.1.1 (protocol 1.0)")).await;
    assert_eq!(o.reasons, vec![Some(DrainReason::Upgrade)]);
}

#[tokio::test(start_paused = true)]
async fn a_runner_that_answers_again_is_told_to_stop_again() {
    let mut c = case("0.1.0 (protocol 1.0)", "0.1.1 (protocol 1.0)");
    c.answers = usize::MAX;
    c.wait_timeout = Some(std::time::Duration::from_secs(2));
    let o = run(c).await;
    assert_eq!(o.result, Ok(1));
    assert!(o.told.len() > 1, "{:?}", o.told);
    assert!(o
        .told
        .iter()
        .all(|t| *t == ("r1".to_string(), DownMode::Drain)));
    assert!(o.reasons.iter().all(|r| *r == Some(DrainReason::Upgrade)));
}

#[tokio::test(start_paused = true)]
async fn wait_timeout_stops_the_wait_with_code_1_and_replaces_nothing() {
    let mut c = busy_case();
    c.running = running(true, true, &["r1"]);
    c.answers = usize::MAX;
    c.wait_timeout = Some(std::time::Duration::from_secs(90));
    let o = run(c).await;
    assert_eq!(o.result, Ok(1));
    assert_eq!((o.hub_stops, o.broker_stops), (0, 0));
    assert!(o.copies.is_empty());
    assert_eq!(
        o.started.len(),
        1,
        "runners that left come back: {:?}",
        o.started
    );
    assert!(!o.lock_left);
}

#[tokio::test(start_paused = true)]
async fn a_wait_that_ends_before_the_timeout_replaces_the_binary() {
    let mut c = busy_case();
    c.answers = 3;
    c.wait_timeout = Some(std::time::Duration::from_secs(90));
    let o = run(c).await;
    assert!(o.result.is_ok(), "{:?}", o.result);
    assert!(!o.copies.is_empty());
}

#[test]
fn parse_duration_rejects_zero_and_garbage() {
    use std::time::Duration;
    assert_eq!(parse_duration("90s"), Ok(Duration::from_secs(90)));
    assert_eq!(parse_duration("15m"), Ok(Duration::from_secs(900)));
    assert_eq!(parse_duration("2h"), Ok(Duration::from_secs(7200)));
    for bad in [
        "", "0s", "0m", "0h", "15", "m", "abc", "-5m", "1.5h", "5d", "1h30m",
    ] {
        assert!(parse_duration(bad).is_err(), "{bad:?} must be rejected");
    }
}

fn job(id: &str, running_min: u64) -> RunningJob {
    RunningJob {
        job: id.into(),
        runner: Some("local:r1".into()),
        running_ms: Some(running_min * 60_000),
    }
}

#[test]
fn the_wait_starts_with_the_count_the_pause_of_new_jobs_and_the_jobs() {
    let (_, lines) = WaitProgress::start(&busy());
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].starts_with("waiting for 1 job to finish"),
        "{lines:?}"
    );
    assert!(lines[0].contains("new jobs are not taken while upgrade waits"));
    assert_eq!(lines[1], "  j1 on r1 (tests)");
}

#[test]
fn the_wait_prints_on_a_change_and_once_a_minute_in_one_line() {
    use std::time::Duration;
    let secs = Duration::from_secs;
    let (mut progress, _) = WaitProgress::start(&busy());
    let one = [job("j1", 12)];
    assert_eq!(progress.observe(&one, secs(5)), None);
    assert_eq!(progress.observe(&one, secs(59)), None);
    assert_eq!(
        progress.observe(&one, secs(60)).as_deref(),
        Some("waiting for 1 job, running 12 min")
    );
    assert_eq!(progress.observe(&one, secs(119)), None);
    assert_eq!(
        progress.observe(&one, secs(120)).as_deref(),
        Some("waiting for 1 job, running 12 min")
    );
    let two = [job("j1", 13), job("j2", 1)];
    assert_eq!(
        progress.observe(&two, secs(125)).as_deref(),
        Some("waiting for 2 jobs, running 13 min"),
        "a new job prints at once, the longest run time is named"
    );
    assert_eq!(progress.observe(&two, secs(130)), None);
    assert_eq!(
        progress.observe(&[job("j2", 1)], secs(135)).as_deref(),
        Some("waiting for 1 job, running 1 min")
    );
    assert_eq!(
        progress.observe(&[], secs(140)).as_deref(),
        Some("the jobs have finished, waiting for the runners to stop")
    );
    assert_eq!(progress.observe(&[], secs(145)), None);
}

#[test]
fn a_long_wait_prints_few_lines_and_the_run_time_has_a_short_unit() {
    use std::time::Duration;
    let (mut progress, _) = WaitProgress::start(&busy());
    let lines = (1..=1800u64)
        .filter_map(|s| progress.observe(&[job("j1", 0)], Duration::from_secs(s)))
        .count();
    assert_eq!(lines, 30, "half an hour is one line a minute");
    let (mut p, _) = WaitProgress::start(&Activity::default());
    let long = RunningJob {
        running_ms: Some(3_900_000),
        ..job("j", 0)
    };
    assert_eq!(
        p.observe(&[long], Duration::from_secs(1)).as_deref(),
        Some("waiting for 1 job, running 1 h 05 min")
    );
    let short = RunningJob {
        running_ms: Some(40_000),
        ..job("k", 0)
    };
    assert_eq!(
        p.observe(&[short], Duration::from_secs(2)).as_deref(),
        Some("waiting for 1 job, running 40 s")
    );
}

fn put(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
}

fn text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn replace_binary_puts_the_new_file_in_place_and_keeps_the_old_aside() {
    use lr_cli::os_service::OsSystem;
    let dir = TempDir::new("replace");
    let (from, to) = (dir.path().join("new"), dir.path().join("localrunner"));
    put(&from, "new");
    put(&to, "old");
    OsSystem
        .apply(&Action::ReplaceBinary {
            from,
            to: to.clone(),
        })
        .unwrap();
    assert_eq!(text(&to), "new");
    assert_eq!(text(&dir.path().join("localrunner.old")), "old");
}

#[test]
fn replace_binary_removes_a_previous_aside_file_first() {
    use lr_cli::os_service::OsSystem;
    let dir = TempDir::new("replace_twice");
    let (from, to) = (dir.path().join("new"), dir.path().join("localrunner"));
    put(&from, "new");
    put(&to, "old");
    put(&dir.path().join("localrunner.old"), "ancient");
    OsSystem
        .apply(&Action::ReplaceBinary {
            from,
            to: to.clone(),
        })
        .unwrap();
    assert_eq!(text(&to), "new");
    assert_eq!(text(&dir.path().join("localrunner.old")), "old");
}

#[test]
fn replace_binary_restores_the_old_file_when_the_copy_fails() {
    use lr_cli::os_service::OsSystem;
    let dir = TempDir::new("replace_fails");
    let to = dir.path().join("localrunner");
    put(&to, "old");
    let failed = OsSystem.apply(&Action::ReplaceBinary {
        from: dir.path().join("missing"),
        to: to.clone(),
    });
    assert!(failed.is_err());
    assert_eq!(text(&to), "old");
    assert!(!dir.path().join("localrunner.old").exists());
}

#[test]
fn replace_binary_installs_when_nothing_is_there_yet() {
    use lr_cli::os_service::OsSystem;
    let dir = TempDir::new("replace_fresh");
    let (from, to) = (dir.path().join("new"), dir.path().join("localrunner"));
    put(&from, "new");
    OsSystem
        .apply(&Action::ReplaceBinary {
            from,
            to: to.clone(),
        })
        .unwrap();
    assert_eq!(text(&to), "new");
    assert!(!dir.path().join("localrunner.old").exists());
}

#[test]
fn a_leftover_aside_file_is_removed_and_a_missing_one_is_not_an_error() {
    use lr_cli::os_service::remove_leftover;
    let dir = TempDir::new("leftover");
    let binary = dir.path().join("localrunner");
    remove_leftover(&binary);
    put(&dir.path().join("localrunner.old"), "old");
    remove_leftover(&binary);
    assert!(!dir.path().join("localrunner.old").exists());
}

#[test]
fn the_plan_says_open_clients_keep_the_old_version() {
    assert!(
        lr_cli::commands::upgrade_cmd::KEEP_OLD_NOTE.contains("keep running on the old version")
    );
}

#[test]
fn the_upgrade_plan_replaces_the_binary_and_never_copies_over_it() {
    use lr_cli::commands::setup::upgrade_plan;
    let plan = upgrade_plan(PathBuf::from("/new/localrunner"), PathBuf::from("/bin"));
    assert!(!plan.is_empty());
    assert!(plan
        .iter()
        .all(|a| matches!(a, Action::ReplaceBinary { .. })));
    assert!(upgrade_plan(PathBuf::from("/bin/localrunner"), PathBuf::from("/bin")).is_empty());
}

fn peer(name: &str, machine: &str, version: Option<&str>, online: bool) -> PoolRunner {
    PoolRunner {
        name: name.into(),
        machine: Some(machine.into()),
        version: version.map(String::from),
        online,
        kind: Kind::Unknown,
    }
}

#[test]
fn plan_lines_list_other_machines_with_versions_and_say_they_are_not_touched() {
    let pool = [
        peer("linux", "ci-1", Some("0.1.0"), true),
        peer("old", "ci-2", None, true),
    ];
    let lines = plan_lines(&pool, "0.1.1");
    let all = lines.join("\n");
    assert!(all.contains("not touched"), "{all}");
    assert!(
        all.contains("linux") && all.contains("ci-1") && all.contains("0.1.0"),
        "{all}"
    );
    assert!(all.contains("old") && all.contains("unknown"), "{all}");
    assert!(plan_lines(&[], "0.1.1").is_empty());
}

#[test]
fn report_lines_name_runners_still_on_another_version() {
    let pool = [
        peer("linux", "ci-1", Some("0.1.0"), true),
        peer("same", "ci-2", Some("0.1.1"), true),
    ];
    let lines = report_lines(&pool, &[], &v("0.1.1 (protocol 1.0)"), false);
    let all = lines.join("\n");
    assert!(
        all.contains("linux") && all.contains("0.1.0") && all.contains("ci-1"),
        "{all}"
    );
    assert!(all.contains("localrunner upgrade"), "{all}");
    assert!(!all.contains("same"), "{all}");
}

#[test]
fn report_lines_say_a_protocol_change_breaks_them() {
    let pool = [peer("linux", "ci-1", Some("0.1.0"), true)];
    let all = report_lines(&pool, &[], &v("0.2.0 (protocol 2.0)"), true).join("\n");
    assert!(all.contains("protocol"), "{all}");
    let quiet = report_lines(&pool, &[], &v("0.1.1 (protocol 1.0)"), false).join("\n");
    assert!(!quiet.contains("protocol"), "{quiet}");
}

#[test]
fn report_lines_mark_container_and_offline_runners_manual_step() {
    let mut boxed = peer("box", "172.17.0.2", Some("0.1.0"), true);
    boxed.kind = Kind::Container;
    let pool = [boxed, peer("away", "ci-3", Some("0.1.0"), false)];
    let all = report_lines(&pool, &[], &v("0.1.1 (protocol 1.0)"), false).join("\n");
    assert!(all.contains("box") && all.contains("manual step"), "{all}");
    assert!(
        all.contains("away") && all.contains("not updated: offline"),
        "{all}"
    );
}

#[test]
fn no_lines_when_every_runner_is_on_the_new_version() {
    let pool = [peer("a", "m1", Some("0.1.1"), true)];
    assert!(report_lines(&pool, &[], &v("0.1.1 (protocol 1.0)"), true).is_empty());
    assert!(report_lines(&[], &[], &v("0.1.1 (protocol 1.0)"), false).is_empty());
}

fn report_of(o: &Outcome) -> String {
    let at = o
        .said
        .iter()
        .position(|l| l.starts_with("upgraded to"))
        .unwrap_or_else(|| panic!("no report in {:?}", o.said));
    o.said[at..].join("\n")
}

#[tokio::test(start_paused = true)]
async fn a_restarted_runner_of_this_installation_is_not_reported_as_still_on_the_old_version() {
    let mut c = case("0.1.0 (protocol 1.0)", "0.1.1 (protocol 1.0)");
    c.pool = vec![
        peer("local:r1", "local:alex", Some("0.1.0"), true),
        peer("linux", "ci-1", Some("0.1.0"), true),
    ];
    c.after_restart = vec![
        peer("local:r1", "local:alex", Some("0.1.1"), true),
        peer("linux", "ci-1", Some("0.1.0"), true),
    ];
    let o = run(c).await;
    assert!(o.result.is_ok(), "{:?}", o.result);
    let said = report_of(&o);
    assert!(!said.contains("r1"), "{said}");
    assert!(said.contains("linux on ci-1: still on 0.1.0"), "{said}");
}

#[tokio::test(start_paused = true)]
async fn a_runner_service_is_not_started_again_nor_waited_for_and_its_restart_is_named() {
    let mut c = case("0.1.0 (protocol 1.0)", "0.1.1 (protocol 1.0)");
    c.service_runner = Some(Some(true));
    c.pool = vec![peer("local:r1", "local:alex", Some("0.1.0"), true)];
    c.after_restart = vec![peer("local:r1", "local:alex", Some("0.1.0"), false)];
    let started = tokio::time::Instant::now();
    let o = run(c).await;
    assert_eq!(o.result, Ok(0), "the upgrade itself succeeded");
    assert!(
        started.elapsed() < RECONNECT_WITHIN,
        "{:?}",
        started.elapsed()
    );
    assert!(o.started.is_empty(), "no user copy: {:?}", o.started);
    let said = o.said.join("\n");
    assert!(
        said.contains("runner r1 runs as the OS service")
            && said.contains("restart it as administrator"),
        "{said}"
    );
    assert!(!said.contains("did not reconnect"), "{said}");
}

#[tokio::test(start_paused = true)]
async fn a_runner_that_did_not_reconnect_is_named() {
    let mut c = case("0.1.0 (protocol 1.0)", "0.1.1 (protocol 1.0)");
    c.pool = vec![peer("local:r1", "local:alex", Some("0.1.0"), true)];
    c.after_restart = vec![peer("local:r1", "local:alex", Some("0.1.0"), false)];
    let started = tokio::time::Instant::now();
    let o = run(c).await;
    assert!(
        started.elapsed() >= RECONNECT_WITHIN,
        "{:?}",
        started.elapsed()
    );
    let said = report_of(&o);
    assert!(said.contains("r1: did not reconnect within 20 s"), "{said}");
    assert!(!said.contains("still on"), "{said}");
}

#[tokio::test(start_paused = true)]
async fn a_hub_that_does_not_answer_ends_the_reconnect_check_within_the_bound() {
    let mut c = case("0.1.0 (protocol 1.0)", "0.1.1 (protocol 1.0)");
    c.pool = vec![peer("local:r1", "local:alex", Some("0.1.0"), true)];
    c.after_restart = vec![peer("local:r1", "local:alex", Some("0.1.1"), true)];
    let mut silent = case("0.1.0 (protocol 1.0)", "0.1.1 (protocol 1.0)");
    silent.pool = c.pool.clone();
    let started = tokio::time::Instant::now();
    run(silent).await;
    let never_back = started.elapsed();
    c.pool_hangs = true;
    let started = tokio::time::Instant::now();
    let o = run(c).await;
    let took = started.elapsed();
    assert!(took >= RECONNECT_WITHIN, "{took:?}");
    assert!(
        took <= never_back,
        "a hub that does not answer ends the check like a runner that is not back: {took:?} against {never_back:?}"
    );
    let said = report_of(&o);
    assert!(said.contains("r1: did not reconnect within 20 s"), "{said}");
}

#[tokio::test(start_paused = true)]
async fn a_runner_back_on_another_version_is_named_with_it() {
    let mut c = case("0.1.0 (protocol 1.0)", "0.1.1 (protocol 1.0)");
    c.pool = vec![peer("local:r1", "local:alex", Some("0.1.0"), true)];
    c.after_restart = vec![peer("local:r1", "local:alex", Some("0.1.0"), true)];
    let o = run(c).await;
    let said = report_of(&o);
    assert!(
        said.contains("r1: reconnected on version 0.1.0, not 0.1.1"),
        "{said}"
    );
    assert!(!said.contains("did not reconnect"), "{said}");
}

#[test]
fn report_lines_name_a_restarted_runner_that_did_not_reconnect() {
    let new = v("0.1.1 (protocol 1.0)");
    let lines = report_lines(
        &[],
        &[
            ("r1".to_string(), Reconnected::No),
            ("r2".to_string(), Reconnected::Yes),
        ],
        &new,
        true,
    );
    let all = lines.join("\n");
    assert!(all.contains("r1: did not reconnect within 20 s"), "{all}");
    assert!(!all.contains("r2") && !all.contains("protocol"), "{all}");
}

#[test]
fn the_runners_of_this_installation_are_not_part_of_the_pool() {
    use lr_cli::commands::upgrade_pool::rows;
    let info = |name: &str, machine: &str, version: &str| {
        let caps = lr_proto::WireCapabilities {
            platforms: Default::default(),
            mem_mb: 0,
            cpu: 0,
            disk_mb: 0,
            programs: Default::default(),
            tags: Default::default(),
            max_job_timeout_s: None,
            services: Default::default(),
            machine: Some(lr_core::MachineRef(machine.into())),
            accept_min_mem_mb: None,
            accept_min_free_disk_mb: None,
            cache_max_mb: None,
            cache_used_mb: None,
            service_mem_mb: Default::default(),
            commands: Default::default(),
            enforcement: None,
            version: Some(version.into()),
            enforce_limit: false,
            programs_missing: Default::default(),
        };
        lr_proto::RunnerInfo {
            name: name.into(),
            online: true,
            caps,
            last_seen: None,
            free_mem_mb: 0,
            free_cpu: 0,
            held_tags: Vec::new(),
            services: Vec::new(),
            unmet: Vec::new(),
            liveness: None,
            silent_for_s: None,
            kind: None,
            pool: None,
            can_start_now: None,
            busy: None,
        }
    };
    let got = rows(
        vec![
            info("r1", "local:alex", "0.1.0"),
            info("local:r1", "local:alex", "0.1.0"),
            info("r1", "ci-9", "0.1.0"),
            info("peer", "ci-1", "0.1.0"),
        ],
        &["r1".to_string()],
    );
    let names: Vec<(&str, Option<&str>)> = got
        .iter()
        .map(|p| (p.name.as_str(), p.machine.as_deref()))
        .collect();
    assert_eq!(names, [("r1", Some("ci-9")), ("peer", Some("ci-1"))]);
}

struct BusySecond {
    applied: Mutex<Vec<String>>,
    busy_left: Mutex<usize>,
}

impl System for BusySecond {
    fn apply(&self, action: &Action) -> Result<(), String> {
        let text = action.to_string();
        if text.contains("second") {
            let mut left = self.busy_left.lock().unwrap();
            if *left > 0 {
                *left -= 1;
                return Err("the file is busy".into());
            }
        }
        self.applied.lock().unwrap().push(text);
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn a_busy_second_binary_does_not_make_the_first_one_replaced_again() {
    let copy = |name: &str| Action::ReplaceBinary {
        from: std::path::PathBuf::from("new").join(name),
        to: std::path::PathBuf::from("installed").join(name),
    };
    let system = BusySecond {
        applied: Mutex::new(Vec::new()),
        busy_left: Mutex::new(3),
    };
    lr_cli::commands::upgrade_cmd::replace_binary(&system, &[copy("first"), copy("second")])
        .await
        .unwrap();
    let applied = system.applied.lock().unwrap().clone();
    assert_eq!(applied.len(), 2, "{applied:?}");
    assert!(applied[0].contains("first") && applied[1].contains("second"));
}

#[tokio::test(start_paused = true)]
async fn upgrade_cancel_lists_the_cancelled_and_the_queued_orders() {
    let mut c = busy_case();
    c.cancel = true;
    let o = run(c).await;
    assert!(o.result.is_ok(), "{:?}", o.result);
    assert!(o.said.iter().any(|l| l == "cancelling: j1"), "{:?}", o.said);
    assert!(
        o.said.iter().any(|l| l == "staying queued: j2, j3"),
        "{:?}",
        o.said
    );
    let waited = run(busy_case()).await;
    assert!(
        !waited.said.iter().any(|l| l.starts_with("cancelling:")),
        "{:?}",
        waited.said
    );
    let lines = busy().lines().join("\n");
    assert!(lines.contains("waiting orders: 2"), "{lines}");
    assert!(lines.contains("j2, j3"), "{lines}");
}

#[tokio::test(start_paused = true)]
async fn upgrade_cancel_names_only_the_jobs_of_the_runners_it_stops() {
    let mut c = busy_case();
    c.cancel = true;
    c.activity.running.push("j9 on r7 (tests)".into());
    c.activity.running_jobs.push(RunningJob {
        job: "j9".into(),
        runner: Some("r7".into()),
        running_ms: None,
    });
    let o = run(c).await;
    assert!(o.result.is_ok(), "{:?}", o.result);
    assert!(o.said.iter().any(|l| l == "cancelling: j1"), "{:?}", o.said);
    assert!(
        o.said.iter().any(|l| l == "still running elsewhere: j9"),
        "{:?}",
        o.said
    );
    let local = run({
        let mut c = busy_case();
        c.cancel = true;
        c
    })
    .await;
    assert!(
        !local
            .said
            .iter()
            .any(|l| l.starts_with("still running elsewhere")),
        "{:?}",
        local.said
    );
}

#[tokio::test(start_paused = true)]
async fn every_page_of_the_queue_is_read_and_listed() {
    let ids: Vec<String> = (1..=120).map(|i| format!("q{i}")).collect();
    let asked = Mutex::new(Vec::new());
    let (all, asked_ref) = (&ids, &asked);
    let read = pages(|cursor: Option<String>| async move {
        asked_ref.lock().unwrap().push(cursor.clone());
        let from: usize = cursor.map_or(0, |c| c.parse().unwrap());
        let to = (from + 50).min(all.len());
        let next = (to < all.len()).then(|| to.to_string());
        Ok((all[from..to].to_vec(), next))
    })
    .await
    .unwrap();
    assert_eq!(read.concat(), ids);
    assert_eq!(
        asked.lock().unwrap().clone(),
        vec![None, Some("50".to_string()), Some("100".to_string())]
    );
    let activity = Activity {
        queued: read.concat(),
        ..Activity::default()
    };
    let lines = activity.cancel_lines(&Default::default());
    assert_eq!(
        lines.last().unwrap(),
        &format!("staying queued: {}", ids.join(", "))
    );
    let stuck = pages(|_cursor: Option<String>| async {
        Ok((vec!["x".to_string()], Some("same".to_string())))
    })
    .await
    .unwrap();
    assert_eq!(
        stuck.len(),
        2,
        "a token that comes back unchanged ends the reading"
    );
}
