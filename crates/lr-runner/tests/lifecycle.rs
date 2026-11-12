mod common;

use common::*;
use lr_abstractions::fakes::{JobScript, StepScript};
use lr_abstractions::{
    Level, ProcessRef, ProcessRunner, RunnerStore, ServiceRow, ServiceState, SpawnSpec,
};
use lr_core::{JobState, Timestamp};
use lr_proto::{BrokerToRunner, ReasonCode, RunnerToBroker, StopService};
use lr_runner::{AgentExit, RunnerSettings};
use std::path::{Path, PathBuf};
use std::time::Duration;

const SEC: Duration = Duration::from_secs(1);

fn row(name: &str, p: &ProcessRef) -> ServiceRow {
    ServiceRow {
        name: name.into(),
        pid: p.pid,
        pid_start: p.start,
        state: ServiceState::Idle,
        started_at: Timestamp(START),
        expires_at: Timestamp(START + 3_600_000),
        mem_mb: 512,
    }
}

#[tokio::test]
async fn orphaned_services_of_a_previous_run_are_killed_at_start() {
    let rig = Rig::new();
    let alive = rig
        .process
        .spawn(SpawnSpec::new("daemon", PathBuf::from("/")))
        .await
        .unwrap();
    rig.store
        .upsert_service(row("alive", &alive))
        .await
        .unwrap();
    let reused = ProcessRef {
        pid: alive.pid,
        start: Some(alive.start.unwrap() + 1),
    };
    rig.store
        .upsert_service(row("reused", &reused))
        .await
        .unwrap();
    let unknown = ProcessRef {
        pid: 77,
        start: None,
    };
    rig.store
        .upsert_service(row("unknown", &unknown))
        .await
        .unwrap();
    let (_c, _h) = rig.start();
    let _b = Broker::accept(&rig).await;
    assert_eq!(
        rig.process.kills(),
        vec![alive],
        "only the same process is killed"
    );
    let kept: Vec<String> = rig
        .store
        .services()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(
        kept,
        vec!["unknown".to_string()],
        "a process not known gone stays listed"
    );
    assert!(rig.log.records().iter().any(
        |r| r.level == Level::Warn && r.fields.contains(&("service".into(), "unknown".into()))
    ));
}

#[tokio::test]
async fn stop_service_kills_the_tree_and_reports() {
    let rig = Rig::new();
    let p = rig
        .process
        .spawn(SpawnSpec::new("daemon", PathBuf::from("/")))
        .await
        .unwrap();
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.until(|m| matches!(m, RunnerToBroker::ServicesReport(_)))
        .await;
    rig.store
        .upsert_service(row("build-daemon", &p))
        .await
        .unwrap();
    b.send(BrokerToRunner::StopService(StopService {
        name: "build-daemon".into(),
    }))
    .await;
    let m = b
        .until(|m| matches!(m, RunnerToBroker::ServicesReport(r) if r.services.is_empty()))
        .await;
    assert!(matches!(m, RunnerToBroker::ServicesReport(_)));
    assert_eq!(rig.process.kills(), vec![p]);
}

#[tokio::test]
async fn services_are_reported_after_welcome() {
    let rig = Rig::new();
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let m = b
        .until(|m| matches!(m, RunnerToBroker::ServicesReport(_)))
        .await;
    assert_eq!(
        m,
        RunnerToBroker::ServicesReport(lr_proto::ServicesReport {
            services: Vec::new()
        })
    );
}

#[tokio::test]
async fn drain_finishes_the_job_then_says_bye() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script("job-1", JobScript::new(vec![StepScript::new(10 * SEC, 0)]));
    let (c, h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 60)], 60),
    )
    .await;
    b.next_job_msg().await.unwrap();
    c.drain().await;
    let m = b.until(|m| matches!(m, RunnerToBroker::Drain)).await;
    assert_eq!(m, RunnerToBroker::Drain);
    rig.advance(10 * SEC).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Passed);
    let bye = b.until(|m| matches!(m, RunnerToBroker::Bye(_))).await;
    assert!(matches!(bye, RunnerToBroker::Bye(_)));
    assert!(matches!(h.await.unwrap(), AgentExit::Stopped));
}

#[tokio::test]
async fn down_cancel_cancels_with_runner_stopped_then_bye() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script("job-1", JobScript::new(vec![StepScript::new(600 * SEC, 0)]));
    let (c, h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 600)], 600),
    )
    .await;
    b.next_job_msg().await.unwrap();
    c.cancel_and_stop().await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Cancelled);
    assert_eq!(r.reason, Some(ReasonCode::RunnerStopped));
    b.until(|m| matches!(m, RunnerToBroker::Bye(_))).await;
    assert!(matches!(h.await.unwrap(), AgentExit::Stopped));
}

#[tokio::test]
async fn drained_runner_refuses_new_assignments() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script("job-1", JobScript::new(vec![StepScript::new(10 * SEC, 0)]));
    let (c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let spec = worktree_spec("/w/tree", vec![step("b", "make", 60)], 60);
    b.assign("job-1", 1, spec.clone()).await;
    b.next_job_msg().await.unwrap();
    c.drain().await;
    b.until(|m| matches!(m, RunnerToBroker::Drain)).await;
    b.assign("job-2", 1, spec).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.job.0, "job-2");
    assert_eq!(r.detail.as_deref(), Some("runner is draining"));
}

#[test]
fn settings_parse_runner_toml_keys() {
    let text = r#"
name = "linux-heavy"
platforms = ["linux", "android"]
tags = ["emulator"]

[accept]
min_mem_mb = 8192

[programs]
git = {}
adb = { check = "adb version" }

[limits]
max_job_timeout_s = 3600

[cache]
max_size_gb = 40
retention_days = 14
evict = "lru"

[workspace]
max_size_gb = 20
retention_days = 7

[grace]
broker_lost_kill_s = 120

[env]
path = "/usr/bin"
"#;
    let s = RunnerSettings::from_toml(text, PathBuf::from("/state")).unwrap();
    assert_eq!(s.name, "linux-heavy");
    assert!(s.platforms.contains("android") && s.tags.contains("emulator"));
    assert_eq!(s.accept_min_mem_mb, Some(8192));
    assert!(s.programs.contains("git") && s.programs.contains("adb"));
    assert_eq!(s.max_job_timeout_s, Some(3600));
    assert_eq!(s.cache.max_size_gb, Some(40));
    assert_eq!(s.cache.retention_days, Some(14));
    assert_eq!(s.workspace.max_size_gb, Some(20));
    assert_eq!(s.workspace.retention_days, Some(7));
    assert_eq!(s.broker_lost_kill_s, Some(120));
    assert_eq!(s.path.as_deref(), Some("/usr/bin"));
    let caps = s.capabilities(None);
    assert_eq!(caps.max_job_timeout_s, Some(3600));
    assert_eq!(caps.cache_max_mb, Some(40 * 1024));
}

#[test]
fn settings_refuse_an_unknown_eviction_and_a_zero_ceiling() {
    let e = RunnerSettings::from_toml("name = \"a\"\n[cache]\nevict = \"random\"\n", "/s".into());
    assert!(e.unwrap_err().to_string().contains("evict"));
    let z = RunnerSettings::from_toml(
        "name = \"a\"\n[limits]\nmax_job_timeout_s = 0\n",
        "/s".into(),
    );
    assert!(z.is_err());
}

#[test]
fn lost_kill_defaults_to_three_minutes_and_only_shortens() {
    let mut s = RunnerSettings::new("a", "/s".into());
    assert_eq!(s.lost_kill_s(None), 180);
    assert_eq!(s.lost_kill_s(Some(300)), 300);
    s.broker_lost_kill_s = Some(60);
    assert_eq!(s.lost_kill_s(Some(300)), 60);
    s.broker_lost_kill_s = Some(900);
    assert_eq!(s.lost_kill_s(Some(300)), 300);
}

#[tokio::test]
async fn bye_is_sent_when_the_broker_comes_back() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script("job-1", JobScript::new(vec![StepScript::new(10 * SEC, 0)]));
    let (c, h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 60)], 60),
    )
    .await;
    b.next_job_msg().await.unwrap();
    rig.net.set_reachable(BROKER, false);
    rig.net.cut(BROKER);
    c.drain().await;
    rig.advance(10 * SEC).await;
    rig.advance(SEC).await;
    assert!(
        rig.pending()
            .await
            .iter()
            .any(|e| e.kind == lr_abstractions::OutboxKind::JobResult),
        "the job finished while the broker was away"
    );
    assert!(!h.is_finished(), "no Bye has been sent, the agent waits");
    rig.net.set_reachable(BROKER, true);
    rig.advance(10 * SEC).await;
    let back = Broker::accept(&rig).await;
    let r = result_of(&back.until_result().await);
    assert_eq!(r.state, JobState::Passed);
    let bye = back.until(|m| matches!(m, RunnerToBroker::Bye(_))).await;
    assert!(matches!(bye, RunnerToBroker::Bye(_)));
    assert!(matches!(h.await.unwrap(), AgentExit::Stopped));
}
