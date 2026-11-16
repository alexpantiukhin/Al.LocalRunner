mod common;

use common::*;
use lr_abstractions::fakes::{JobScript, StepScript};
use lr_abstractions::{
    Level, OutboxKind, ProcessRef, ProcessRunner, RunnerStore, ServiceRow, ServiceState, SpawnSpec,
};
use lr_core::{JobId, JobState, Timestamp};
use lr_proto::{BrokerToRunner, Chunk, ReasonCode, RunnerToBroker, StopService};
use lr_runner::{AgentExit, RunnerSettings, STOP_BOUND};
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
    c.drain(lr_proto::DrainReason::Down).await;
    let m = b.until(|m| matches!(m, RunnerToBroker::Drain { .. })).await;
    assert_eq!(
        m,
        RunnerToBroker::Drain {
            reason: Some(lr_proto::DrainReason::Down)
        }
    );
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
    c.cancel_and_stop(lr_proto::DrainReason::Down).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Cancelled);
    assert_eq!(r.reason, Some(ReasonCode::RunnerStopped));
    assert_eq!(r.detail.as_deref(), Some("stopped by down"));
    b.until(|m| matches!(m, RunnerToBroker::Bye(_))).await;
    assert!(matches!(h.await.unwrap(), AgentExit::Stopped));
}

#[tokio::test]
async fn a_cancel_for_an_upgrade_names_upgrade_in_the_stop_detail() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script("job-1", JobScript::new(vec![StepScript::new(600 * SEC, 0)]));
    let (c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 600)], 600),
    )
    .await;
    b.next_job_msg().await.unwrap();
    c.cancel_and_stop(lr_proto::DrainReason::Upgrade).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Cancelled);
    assert_eq!(r.reason, Some(ReasonCode::RunnerStopped));
    assert_eq!(r.detail.as_deref(), Some("stopped by upgrade"));
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
    c.drain(lr_proto::DrainReason::Down).await;
    b.until(|m| matches!(m, RunnerToBroker::Drain { .. })).await;
    b.assign("job-2", 1, spec).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.job.0, "job-2");
    assert_eq!(r.detail.as_deref(), Some("runner is draining"));
    assert_eq!(r.reason, Some(ReasonCode::RunnerDraining));
}

#[tokio::test]
async fn drain_carries_its_reason_to_the_broker() {
    let rig = Rig::new();
    let (c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    c.drain(lr_proto::DrainReason::Upgrade).await;
    let m = b.until(|m| matches!(m, RunnerToBroker::Drain { .. })).await;
    assert_eq!(
        m,
        RunnerToBroker::Drain {
            reason: Some(lr_proto::DrainReason::Upgrade)
        }
    );
}

fn upgrade_flag(live: bool) -> Arc<std::sync::atomic::AtomicBool> {
    Arc::new(std::sync::atomic::AtomicBool::new(live))
}

#[tokio::test]
async fn a_runner_drained_for_an_upgrade_that_ended_takes_work_again() {
    let mut rig = Rig::new();
    rig.upgrade_live = Some(upgrade_flag(false));
    rig.files.mkdir(std::path::Path::new("/w/tree"));
    rig.script(
        "job-2",
        JobScript::new(vec![StepScript::new(std::time::Duration::ZERO, 0)]),
    );
    let (c, h) = rig.start();
    let b = Broker::accept(&rig).await;
    c.drain(lr_proto::DrainReason::Upgrade).await;
    b.until(|m| matches!(m, RunnerToBroker::Drain { .. })).await;
    while b.recv().await.is_some() {}
    rig.advance(Duration::from_secs(2)).await;
    let b2 = Broker::accept(&rig).await;
    assert!(!h.is_finished(), "the runner did not leave");
    b2.assign(
        "job-2",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 60)], 60),
    )
    .await;
    let r = result_of(&b2.until_result().await);
    assert_eq!(r.job.0, "job-2");
    assert_ne!(
        r.reason,
        Some(ReasonCode::RunnerDraining),
        "it is not draining any more"
    );
}

#[tokio::test]
async fn a_runner_drained_for_an_upgrade_resumes_when_the_lock_is_gone_while_its_job_runs() {
    let mut rig = Rig::new();
    let flag = upgrade_flag(true);
    rig.upgrade_live = Some(flag.clone());
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script("job-1", JobScript::new(vec![StepScript::new(30 * SEC, 0)]));
    rig.script(
        "job-3",
        JobScript::new(vec![StepScript::new(Duration::ZERO, 0)]),
    );
    let spec = worktree_spec("/w/tree", vec![step("b", "make", 120)], 120);
    let (c, h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, spec.clone()).await;
    b.next_job_msg().await.unwrap();
    c.drain(lr_proto::DrainReason::Upgrade).await;
    b.until(|m| matches!(m, RunnerToBroker::Drain { .. })).await;
    flag.store(false, std::sync::atomic::Ordering::SeqCst);
    rig.advance(16 * SEC).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while b.recv().await.is_some() {}
    })
    .await
    .expect("the runner did not close its drained session");
    assert!(rig
        .logs_text()
        .contains("the upgrade ended without replacing this runner, resuming"));
    rig.advance(2 * SEC).await;
    let b2 = Broker::accept(&rig).await;
    b2.assign("job-2", 1, spec.clone()).await;
    let r = result_of(&b2.until_result().await);
    assert_eq!(r.job.0, "job-2");
    assert_ne!(
        r.reason,
        Some(ReasonCode::RunnerDraining),
        "it is not draining any more"
    );
    assert_eq!(
        r.detail.as_deref(),
        Some("runner is busy with job job-1"),
        "job-1 still runs"
    );
    rig.advance(15 * SEC).await;
    let r = result_of(&b2.until_result().await);
    assert_eq!((r.job.0.as_str(), r.state), ("job-1", JobState::Passed));
    b2.assign("job-3", 1, spec).await;
    let r = result_of(&b2.until_result().await);
    assert_eq!((r.job.0.as_str(), r.state), ("job-3", JobState::Passed));
    assert!(!h.is_finished(), "the runner did not leave");
}

fn drain_levels(rig: &Rig) -> Vec<Level> {
    rig.log
        .records()
        .into_iter()
        .filter(|r| r.message == "drain requested")
        .map(|r| r.level)
        .collect()
}

async fn busy_runner(rig: &Rig) -> (lr_runner::AgentControl, Broker) {
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script("job-1", JobScript::new(vec![StepScript::new(600 * SEC, 0)]));
    let (c, _h) = rig.start();
    let b = Broker::accept(rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 600)], 600),
    )
    .await;
    b.next_job_msg().await.unwrap();
    (c, b)
}

async fn drain_and_wait(c: &lr_runner::AgentControl, b: &Broker, reason: lr_proto::DrainReason) {
    c.drain(reason).await;
    b.until(|m| matches!(m, RunnerToBroker::Drain { .. })).await;
}

#[tokio::test]
async fn repeated_drain_requests_with_one_reason_log_once_at_info() {
    let mut rig = Rig::new();
    rig.upgrade_live = Some(upgrade_flag(true));
    let (c, b) = busy_runner(&rig).await;
    for _ in 0..3 {
        drain_and_wait(&c, &b, lr_proto::DrainReason::Upgrade).await;
    }
    assert_eq!(
        drain_levels(&rig),
        vec![Level::Info, Level::Debug, Level::Debug]
    );
}

#[tokio::test]
async fn a_changed_reason_logs_again() {
    let mut rig = Rig::new();
    rig.upgrade_live = Some(upgrade_flag(true));
    let (c, b) = busy_runner(&rig).await;
    drain_and_wait(&c, &b, lr_proto::DrainReason::Down).await;
    drain_and_wait(&c, &b, lr_proto::DrainReason::Down).await;
    drain_and_wait(&c, &b, lr_proto::DrainReason::Upgrade).await;
    assert_eq!(
        drain_levels(&rig),
        vec![Level::Info, Level::Debug, Level::Info]
    );
}

#[tokio::test]
async fn a_runner_drained_for_an_upgrade_that_still_runs_leaves() {
    let mut rig = Rig::new();
    rig.upgrade_live = Some(upgrade_flag(true));
    let (c, h) = rig.start();
    let b = Broker::accept(&rig).await;
    c.drain(lr_proto::DrainReason::Upgrade).await;
    b.until(|m| matches!(m, RunnerToBroker::Bye(_))).await;
    h.await.unwrap();
}

#[tokio::test]
async fn a_plain_down_leaves_even_when_no_upgrade_runs() {
    let mut rig = Rig::new();
    rig.upgrade_live = Some(upgrade_flag(false));
    let (c, h) = rig.start();
    let b = Broker::accept(&rig).await;
    c.drain(lr_proto::DrainReason::Down).await;
    b.until(|m| matches!(m, RunnerToBroker::Bye(_))).await;
    h.await.unwrap();
}

#[tokio::test]
async fn lifecycle_events_are_logged() {
    let rig = Rig::new();
    let (c, h) = rig.start();
    let b = Broker::accept(&rig).await;
    c.drain(lr_proto::DrainReason::Upgrade).await;
    b.until(|m| matches!(m, RunnerToBroker::Bye(_))).await;
    h.await.unwrap();
    let text = rig.logs_text();
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle} in {text}"))
    };
    assert!(at("runner started") < at("drain requested"));
    assert!(at("drain requested") < at("drain finished, leaving"));
    assert!(text.contains("upgrade"));
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
    c.drain(lr_proto::DrainReason::Down).await;
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
    rig.advance(4 * SEC).await;
    let back = Broker::accept(&rig).await;
    let r = result_of(&back.until_result().await);
    assert_eq!(r.state, JobState::Passed);
    let bye = back.until(|m| matches!(m, RunnerToBroker::Bye(_))).await;
    assert!(matches!(bye, RunnerToBroker::Bye(_)));
    assert!(matches!(h.await.unwrap(), AgentExit::Stopped));
}

async fn fill_outbox(rig: &Rig, rows: usize) {
    let job = JobId("job-old".into());
    for _ in 0..rows {
        let m = RunnerToBroker::Chunk(Chunk {
            job: job.clone(),
            seq: 0,
            step: Some(0),
            text: "x".into(),
        });
        rig.store
            .push_outbox(
                &job,
                OutboxKind::Chunk,
                serde_json::to_string(&m).unwrap(),
                Timestamp(START),
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn stop_request_interrupts_a_stuck_flush_and_keeps_the_outbox() {
    let rig = Rig::new();
    fill_outbox(&rig, 3000).await;
    let (c, h) = rig.start();
    let _b = Broker::accept(&rig).await;
    rig.gate.limit_pipe(50);
    c.drain(lr_proto::DrainReason::Down).await;
    rig.advance(SEC).await;
    assert!(rig.gate.sent() >= 50, "the replay began");
    assert!(!h.is_finished(), "the runner waits for its link first");
    rig.advance(STOP_BOUND).await;
    assert!(h.is_finished(), "the stuck runner did not stop");
    assert_eq!(h.await.unwrap(), AgentExit::Stopped);
    assert_eq!(rig.pending().await.len(), 3000, "the outbox is kept");
}

#[tokio::test]
async fn cancel_and_stop_with_a_stuck_send_stops_too() {
    let rig = Rig::new();
    fill_outbox(&rig, 3000).await;
    let (c, h) = rig.start();
    let _b = Broker::accept(&rig).await;
    rig.gate.stall_sends(true);
    c.cancel_and_stop(lr_proto::DrainReason::Down).await;
    rig.advance(STOP_BOUND + SEC).await;
    assert!(h.is_finished(), "the stuck runner did not stop");
    assert_eq!(h.await.unwrap(), AgentExit::Stopped);
    assert_eq!(rig.pending().await.len(), 3000, "the outbox is kept");
}

#[tokio::test]
async fn a_running_job_keeps_the_runner_alive_while_the_send_is_stuck() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script("job-1", JobScript::new(vec![StepScript::new(30 * SEC, 0)]));
    let (c, h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 60)], 60),
    )
    .await;
    b.next_job_msg().await.unwrap();
    rig.gate.stall_sends(true);
    c.drain(lr_proto::DrainReason::Down).await;
    rig.advance(STOP_BOUND + SEC).await;
    assert!(!h.is_finished(), "the runner left while its job runs");
    rig.advance(19 * SEC).await;
    assert!(!h.is_finished(), "the runner left as soon as its job ended");
    rig.advance(STOP_BOUND + SEC).await;
    assert!(
        h.is_finished(),
        "the stuck runner did not stop after its job"
    );
    assert_eq!(h.await.unwrap(), AgentExit::Stopped);
    assert!(
        rig.pending()
            .await
            .iter()
            .any(|e| e.kind == OutboxKind::JobResult),
        "the result waits in the outbox"
    );
}

#[tokio::test]
async fn an_idle_runner_whose_broker_reads_but_never_acks_stops_within_the_bound() {
    let rig = Rig::new();
    fill_outbox(&rig, 100).await;
    let (c, h) = rig.start();
    let b = Broker::accept(&rig).await;
    let mut seen = 0;
    while seen < 100 {
        if let Some(RunnerToBroker::Chunk(_)) = b.recv().await {
            seen += 1;
        }
    }
    c.drain(lr_proto::DrainReason::Down).await;
    b.until(|m| matches!(m, RunnerToBroker::Drain { .. })).await;
    rig.advance(SEC).await;
    assert!(!h.is_finished(), "the runner gives its broker time first");
    rig.advance(STOP_BOUND).await;
    assert!(h.is_finished(), "an unacknowledged outbox kept the runner");
    assert_eq!(h.await.unwrap(), AgentExit::Stopped);
    assert_eq!(rig.pending().await.len(), 100, "the outbox is kept");
}

#[tokio::test]
async fn an_idle_runner_with_an_undelivered_outbox_and_no_broker_stops_within_the_bound() {
    let rig = Rig::new();
    fill_outbox(&rig, 10).await;
    rig.net.set_reachable(BROKER, false);
    let (c, h) = rig.start();
    rig.settle().await;
    c.drain(lr_proto::DrainReason::Down).await;
    rig.advance(SEC).await;
    assert!(!h.is_finished(), "the runner waits for its broker first");
    rig.advance(STOP_BOUND).await;
    assert!(
        h.is_finished(),
        "the runner waits for a broker it cannot reach"
    );
    assert_eq!(h.await.unwrap(), AgentExit::Stopped);
    assert_eq!(rig.pending().await.len(), 10, "the outbox is kept");
}

#[tokio::test]
async fn idle_runner_told_to_stop_exits_even_when_the_broker_is_unreachable() {
    let rig = Rig::new();
    let (c, h) = rig.start();
    rig.settle().await;
    c.drain(lr_proto::DrainReason::Down).await;
    for _ in 0..40 {
        if h.is_finished() {
            break;
        }
        rig.advance(10 * SEC).await;
    }
    assert!(
        h.is_finished(),
        "the idle runner waits for a broker it cannot reach"
    );
    assert!(matches!(h.await.unwrap(), AgentExit::Stopped));
}
