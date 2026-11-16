use crate::common::TempDir;
use crate::kit::*;
use crate::support::*;
use lr_cli::commands::down_cmd::down;
use lr_cli::commands::setup::{uninstall_with, ServiceRights, UninstallFlags};
use lr_cli::os_service::{Action, System};
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, start_runners, Running};
use lr_core::JobState;
use lr_proto::{ReasonCode, RejectReason};
use lr_runner::AgentExit;
use std::path::Path;
use std::sync::Mutex;

fn file_exists_check(path: &Path) -> String {
    if cfg!(windows) {
        format!("if exist \"{}\" (exit 0) else (exit 1)", path.display())
    } else {
        format!("test -f '{}'", path.display())
    }
}

fn warm_service_toml(dir: &Path, gate: &Gate) -> String {
    let ready = dir.join("svc.ready");
    format!(
        "{}\n[service.svc]\nstart = {}\nready_check = {}\nstop = \"exit 0\"\n\
         ready_timeout_s = 60\nstop_timeout_s = 1\nmemory_mb = 64\nlifetime = \"keep\"\n\
         max_lifetime_s = 600\n",
        runner_toml(),
        toml_str(&gate.command_with(Mode::Tree, "svc", Some(&ready))),
        toml_str(&file_exists_check(&ready)),
    )
}

async fn exits(runner: Running) -> Vec<AgentExit> {
    tokio::time::timeout(HANG, runner.wait())
        .await
        .expect("the runner ends")
}

async fn offline(layout: &Layout, name: &str) {
    until_hub(layout, &format!("{name} offline"), || async {
        let all = runners(layout).await?;
        (!all.iter().any(|r| r.online && r.name.ends_with(name))).then_some(())
    })
    .await;
}

// scenario: 16.24
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drain_finishes_the_job_stops_services_then_says_bye() {
    let dir = TempDir::new("e2e-drain");
    let gate = Gate::new().await;
    let svc_gate = Gate::new().await;
    let release = Release::new(&[&gate, &svc_gate]);
    let toml = warm_service_toml(dir.path(), &svc_gate);
    let (main, mut roles) = machine(&dir, "main", "", &["r1"], &toml).await;
    let profiles = format!(
        "{}\n{}\n{}",
        held_profile("work", &gate, 120, "mem_mb = 64\nservices = [\"svc\"]\n"),
        echo_profile("late", "never-runs", 60, "mem_mb = 64\n"),
        held_profile("hold", &gate, 120, "mem_mb = 64\n"),
    );
    let repo = repo(&dir, &profiles);

    let (work_client, work) = submit(&main, &repo, "work", None).await;
    let service = [
        svc_gate.started("svc", "child").await,
        svc_gate.started("svc", "grandchild").await,
    ];
    let processes = [service[0].process(), service[1].process()];
    gate.started("work", "child").await;

    assert_eq!(down(&main, vec!["r1".into()], false).await, Ok(0));
    let (late_client, late) = submit(&main, &repo, "late", None).await;
    let (code, detail) = until_rejected(late_client, &late, HANG).await;
    assert_eq!(
        code,
        ReasonCode::Rejected(RejectReason::NoRunnerOnMachine),
        "a draining runner takes no new order: {detail}"
    );
    assert!(
        processes.iter().all(|&p| alive(p)),
        "the service lives while the job runs"
    );

    gate.open();
    let ended = until_exit(work_client, &work, HANG).await;
    assert_eq!(
        ended.state,
        JobState::Passed,
        "the job is finished: {ended:?}"
    );
    for p in &service {
        p.gone("the warm service to be stopped").await;
    }
    let bye = exits(roles.take_last()).await;
    assert!(matches!(bye.as_slice(), [AgentExit::Stopped]), "{bye:?}");
    offline(&main, "r1").await;

    let again = start_runners(&main, Some("r1"), 1, None, false)
        .await
        .expect("r1 starts again");
    runner_online(&main, "r1").await;
    let (hold_client, hold) = submit(&main, &repo, "hold", None).await;
    let held = gate.started("hold", "child").await;
    assert_eq!(down(&main, vec!["r1".into()], true).await, Ok(0));
    let cancelled = until_exit(hold_client, &hold, HANG).await;
    assert_eq!(cancelled.state, JobState::Cancelled, "{cancelled:?}");
    assert_eq!(
        cancelled.reason,
        Some(ReasonCode::RunnerStopped),
        "{cancelled:?}"
    );
    held.gone("the cancelled job tree").await;
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
            Err(e) if started.elapsed() > HANG => return Err(e.to_string()),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
}

fn schema_version(db: &Path) -> u32 {
    rusqlite::Connection::open(db)
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

fn sql(db: &Path, batch: &str) {
    rusqlite::Connection::open(db)
        .and_then(|c| c.execute_batch(batch))
        .unwrap_or_else(|e| panic!("{}: {e}", db.display()));
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
    let ended = until_exit(client, &job, HANG).await;
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
    runner.stopped().await;
    hub.stopped().await;
    broker.stopped().await;

    let hub_db = main.root.join("hub.db");
    let broker_db = main.root.join("broker.db");
    sql(
        &hub_db,
        "ALTER TABLE active_order DROP COLUMN spec_json; DROP TABLE hub_meta; \
         PRAGMA user_version = 1;",
    );
    sql(&broker_db, "PRAGMA user_version = 99;");
    let refused = start_broker(&main, false)
        .await
        .err()
        .expect("a newer broker.db stops the start");
    assert!(refused.contains("is newer"), "{refused}");
    assert_eq!(schema_version(&broker_db), 99, "left untouched");
    sql(
        &broker_db,
        "ALTER TABLE job DROP COLUMN peak_mem_mb; ALTER TABLE job DROP COLUMN peak_disk_mb;          ALTER TABLE runner DROP COLUMN enforcement; ALTER TABLE runner DROP COLUMN version; PRAGMA user_version = 1;",
    );

    let [broker, hub, runner] = start_all(&main).await;
    let backup = main.root.join("hub.db.bak-1");
    assert!(backup.is_file(), "a backup before the migration");
    assert_eq!(schema_version(&backup), 1);
    assert_eq!(schema_version(&hub_db), 2);
    assert!(main.root.join("broker.db.bak-1").is_file());
    assert_eq!(schema_version(&broker_db), 3);
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
    let no_service = ServiceRights {
        installed: false,
        elevated: false,
    };
    let keep = UninstallFlags {
        dir: Some(bin.clone()),
        ..UninstallFlags::default()
    };
    let code = uninstall_with(
        &main,
        keep,
        no_service,
        down(&main, Vec::new(), true),
        &kept,
    )
    .await;
    assert_eq!(code, Ok(0));
    tokio::time::timeout(HANG, roles)
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
    let flags = UninstallFlags {
        purge: true,
        yes: true,
        dir: Some(bin),
        dry_run: false,
    };
    let code = uninstall_with(&main, flags, no_service, async { Ok(0) }, &purge).await;
    assert_eq!(code, Ok(0));
    assert!(!main.root.exists(), "--purge --yes deletes the state");

    remove(dir).await;
}

struct DownOnDrop(Layout);

impl Drop for DownOnDrop {
    fn drop(&mut self) {
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_localrunner"))
            .args(["down", "--cancel", "--yes", "--state-dir"])
            .arg(&self.0.root)
            .arg("--config-dir")
            .arg(&self.0.config)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn up_all_without_init_starts_the_hub_once_the_broker_answers() {
    let dir = TempDir::new("e2e-upall");
    let fresh = layout(&dir, "fresh", "");
    add_runner(&fresh, "r1");
    assert!(!fresh.root.join("broker.db").exists());
    assert!(!fresh.root.join("roles").exists());
    let down_after = DownOnDrop(fresh.clone());
    let up = tokio::process::Command::new(env!("CARGO_BIN_EXE_localrunner"))
        .args(["up", "all", "--state-dir"])
        .arg(&fresh.root)
        .arg("--config-dir")
        .arg(&fresh.config)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("the binary runs");
    let up = tokio::time::timeout(HANG, up.wait_with_output())
        .await
        .expect("up returns")
        .expect("up output");
    assert_eq!(
        up.status.code(),
        Some(0),
        "{}\n{}",
        String::from_utf8_lossy(&up.stdout),
        String::from_utf8_lossy(&up.stderr)
    );
    runner_online(&fresh, "r1").await;
    let listed = brokers(&fresh).await;
    assert!(
        listed.iter().any(|b| b.name == "local"),
        "the hub started after the broker answered lists it: {listed:?}"
    );

    let repo = repo(
        &dir,
        &echo_profile("here", "on-this-machine", 60, "mem_mb = 256\n"),
    );
    let (client, job) = submit(&fresh, &repo, "here", None).await;
    let ended = until_exit(client, &job, HANG).await;
    assert_eq!(ended.state, JobState::Passed, "{ended:?}");
    assert!(ended.output.contains("on-this-machine"), "{}", ended.output);

    drop(down_after);
    remove(dir).await;
}
