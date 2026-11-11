use crate::common::TempDir;
use crate::helper::{helper_command, Mode};
use crate::kit::*;
use crate::support::*;
use lr_cli::commands::down_cmd::down;
use lr_cli::commands::setup::uninstall_with;
use lr_cli::os_service::{Action, System};
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, start_runners, Running};
use lr_core::JobState;
use lr_proto::{ReasonCode, RejectReason};
use lr_runner::AgentExit;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

const ENDS: Duration = Duration::from_secs(90);

fn file_exists_check(path: &Path) -> String {
    if cfg!(windows) {
        format!("if exist \"{}\" (exit 0) else (exit 1)", path.display())
    } else {
        format!("test -f '{}'", path.display())
    }
}

fn warm_service_toml(dir: &Path, gate: &Path) -> String {
    let pids = pid_file(dir, "svc");
    let child = dir.join("svc.pid.child");
    format!(
        "{}\n[service.svc]\nstart = {}\nready_check = {}\nstop = \"exit 0\"\n\
         ready_timeout_s = 60\nstop_timeout_s = 1\nmemory_mb = 64\nlifetime = \"keep\"\n\
         max_lifetime_s = 600\n",
        runner_toml(),
        toml_str(&helper_command(Mode::Tree, &pids, gate)),
        toml_str(&file_exists_check(&child)),
    )
}

async fn exits(runner: Running) -> Vec<AgentExit> {
    tokio::time::timeout(ENDS, runner.wait())
        .await
        .expect("the runner ends")
}

async fn offline(layout: &Layout, name: &str) {
    eventually(
        &format!("{name} offline"),
        Duration::from_secs(60),
        || async {
            let all = runners(layout).await?;
            (!all.iter().any(|r| r.online && r.name.ends_with(name))).then_some(())
        },
    )
    .await;
}

// scenario: 16.24
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drain_finishes_the_job_stops_services_then_says_bye() {
    let dir = TempDir::new("e2e-drain");
    let gate = dir.path().join("gate");
    let svc_gate = dir.path().join("svc-gate");
    let mut release = Release::new(&[&gate, &svc_gate]);
    let toml = warm_service_toml(dir.path(), &svc_gate);
    let (main, mut roles) = machine(&dir, "main", "", &["r1"], &toml).await;
    let profiles = format!(
        "{}\n{}\n{}",
        held_profile(
            "work",
            dir.path(),
            &gate,
            120,
            "mem_mb = 64\nservices = [\"svc\"]\n"
        ),
        echo_profile("late", "never-runs", 60, "mem_mb = 64\n"),
        held_profile("hold", dir.path(), &gate, 120, "mem_mb = 64\n"),
    );
    let repo = repo(&dir, &profiles);

    let (work_client, work) = submit(&main, &repo, "work", None).await;
    let service = [
        started(dir.path(), "svc", "child").await,
        started(dir.path(), "svc", "grandchild").await,
    ];
    for p in service {
        release.watch(p);
    }
    let step = started(dir.path(), "work", "child").await;
    release.watch(step);

    assert_eq!(down(&main, vec!["r1".into()], false).await, Ok(0));
    let (late_client, late) = submit(&main, &repo, "late", None).await;
    let (code, detail) = until_rejected(late_client, &late, ENDS).await;
    assert_eq!(
        code,
        ReasonCode::Rejected(RejectReason::NoRunnerOnMachine),
        "a draining runner takes no new order: {detail}"
    );
    assert!(
        service.iter().all(|&(p, s)| alive(p, s)),
        "the service lives while the job runs"
    );

    open(&gate);
    let ended = until_exit(work_client, &work, ENDS).await;
    assert_eq!(
        ended.state,
        JobState::Passed,
        "the job is finished: {ended:?}"
    );
    gone("the warm service to be stopped", &service).await;
    let bye = exits(roles.take_last()).await;
    assert!(matches!(bye.as_slice(), [AgentExit::Stopped]), "{bye:?}");
    offline(&main, "r1").await;

    std::fs::remove_file(&gate).unwrap();
    let again = start_runners(&main, Some("r1"), 1, None, false)
        .await
        .expect("r1 starts again");
    runner_online(&main, "r1").await;
    let (hold_client, hold) = submit(&main, &repo, "hold", None).await;
    let held = started(dir.path(), "hold", "child").await;
    release.watch(held);
    assert_eq!(down(&main, vec!["r1".into()], true).await, Ok(0));
    let cancelled = until_exit(hold_client, &hold, ENDS).await;
    assert_eq!(cancelled.state, JobState::Cancelled, "{cancelled:?}");
    assert_eq!(
        cancelled.reason,
        Some(ReasonCode::RunnerStopped),
        "{cancelled:?}"
    );
    gone("the cancelled job tree", &[held]).await;
    let bye = exits(again).await;
    assert!(matches!(bye.as_slice(), [AgentExit::Stopped]), "{bye:?}");
    offline(&main, "r1").await;

    roles.stop();
    drop(release);
    remove(dir).await;
}

#[derive(Default)]
struct Recorder {
    applied: Mutex<Vec<Action>>,
    purge: bool,
}

impl System for Recorder {
    fn apply(&self, action: &Action) -> Result<(), String> {
        self.applied.lock().unwrap().push(action.clone());
        match action {
            Action::RemoveDirAll(dir) if self.purge => remove_when_closed(dir),
            _ => Ok(()),
        }
    }
}

fn remove_when_closed(dir: &Path) -> Result<(), String> {
    let started = std::time::Instant::now();
    loop {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return Ok(()),
            Err(e) if started.elapsed() > Duration::from_secs(20) => return Err(e.to_string()),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn schema_version(db: &Path) -> u32 {
    rusqlite::Connection::open(db)
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

async fn sql(db: &Path, batch: &str) {
    eventually(
        &format!("{} to be free", db.display()),
        Duration::from_secs(30),
        || {
            let done = rusqlite::Connection::open(db)
                .and_then(|c| c.execute_batch(batch))
                .is_ok();
            async move { done.then_some(()) }
        },
    )
    .await;
}

async fn start_all(layout: &Layout) -> [Running; 3] {
    let broker = start_broker(layout, false).await.expect("broker starts");
    let hub = start_hub(layout, false).await.expect("hub starts");
    let runner = start_runners(layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    runner_online(layout, "r1").await;
    [broker, hub, runner]
}

async fn passes(layout: &Layout, repo: &Path, profile: &str) {
    let (client, job) = submit(layout, repo, profile, None).await;
    let ended = until_exit(client, &job, ENDS).await;
    assert_eq!(ended.state, JobState::Passed, "{ended:?}");
}

// scenario: 16.10
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upgrade_with_an_older_database_keeps_a_backup_and_uninstall_keeps_state() {
    let dir = TempDir::new("e2e-upg");
    let main = layout(&dir, "main", "");
    add_runner(&main, "r1");
    let repo = repo(
        &dir,
        &echo_profile("quick", "a-run-of-this-node", 60, "mem_mb = 64\n"),
    );
    let [broker, hub, runner] = start_all(&main).await;
    passes(&main, &repo, "quick").await;
    runner.stop();
    hub.stop();
    broker.stop();

    let hub_db = main.root.join("hub.db");
    let broker_db = main.root.join("broker.db");
    sql(
        &hub_db,
        "ALTER TABLE active_order DROP COLUMN spec_json; DROP TABLE hub_meta; \
         PRAGMA user_version = 1;",
    )
    .await;
    sql(&broker_db, "PRAGMA user_version = 99;").await;
    let refused = start_broker(&main, false)
        .await
        .err()
        .expect("a newer broker.db stops the start");
    assert!(refused.contains("is newer"), "{refused}");
    assert_eq!(schema_version(&broker_db), 99, "left untouched");
    sql(&broker_db, "PRAGMA user_version = 1;").await;

    let [broker, hub, runner] = start_all(&main).await;
    let backup = main.root.join("hub.db.bak-1");
    assert!(backup.is_file(), "a backup before the migration");
    assert_eq!(schema_version(&backup), 1);
    assert_eq!(schema_version(&hub_db), 2);
    passes(&main, &repo, "quick").await;
    assert_eq!(
        history(&main).await.len(),
        2,
        "history survives the upgrade"
    );

    let bin = dir.path().join("bin");
    let kept = Recorder::default();
    let roles = tokio::spawn(async move {
        let _ = runner.wait().await;
        let _ = hub.wait().await;
        let _ = broker.wait().await;
    });
    let code = uninstall_with(
        &main,
        false,
        false,
        Some(bin.clone()),
        false,
        down(&main, Vec::new(), true),
        &kept,
    )
    .await;
    assert_eq!(code, Ok(0));
    tokio::time::timeout(ENDS, roles)
        .await
        .expect("every role stops")
        .unwrap();
    let applied = kept.applied.lock().unwrap().clone();
    assert!(
        applied.contains(&Action::RemoveFromUserPath(bin.clone())),
        "{applied:?}"
    );
    assert!(
        !applied.iter().any(|a| matches!(a, Action::RemoveDirAll(_))),
        "{applied:?}"
    );
    for kept in ["machine-id", "broker.db", "hub.db"] {
        assert!(main.root.join(kept).is_file(), "{kept} stays");
    }
    assert!(main.runner_dir("r1").is_dir(), "the runner's keys stay");

    let purge = Recorder {
        purge: true,
        ..Recorder::default()
    };
    let code = uninstall_with(&main, true, true, Some(bin), false, async { Ok(0) }, &purge).await;
    assert_eq!(code, Ok(0));
    assert!(!main.root.exists(), "--purge --yes deletes the state");

    remove(dir).await;
}
