mod common;

use common::*;
use lr_abstractions::fakes::{JobScript, StepScript, FAKE_STEP_PID};
use lr_abstractions::{Clock, DEFAULT_CANCEL_GRACE};
use lr_core::{JobId, JobState, StepState};
use lr_proto::{
    BrokerToRunner, ProtocolError, ProtocolErrorCode, ReasonCode, RunnerToBroker, SettingsUpdate,
};
use lr_runner::AgentExit;
use std::path::Path;
use std::time::Duration;

const SEC: Duration = Duration::from_secs(1);

async fn has_result(rig: &Rig) -> bool {
    rig.pending()
        .await
        .iter()
        .any(|e| e.kind == lr_abstractions::OutboxKind::JobResult)
}

fn tree(rig: &Rig) {
    rig.files.mkdir(Path::new("/w/tree"));
}

#[tokio::test]
async fn job_runs_and_result_reaches_broker_once_after_reconnect() {
    let rig = Rig::new();
    tree(&rig);
    rig.script(
        "job-1",
        JobScript::new(vec![StepScript::new(10 * SEC, 0).output(b"hello\n")]),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("build", "make", 60)], 60),
    )
    .await;
    let first = b.next_job_msg().await.unwrap();
    assert!(matches!(first, RunnerToBroker::Started(ref s) if s.seq == 1));
    rig.net.cut(BROKER);
    rig.advance(10 * SEC).await;
    rig.advance(SEC).await;
    let b2 = Broker::accept(&rig).await;
    let msgs = b2.until_result().await;
    let results: Vec<_> = msgs
        .iter()
        .filter(|m| matches!(m, RunnerToBroker::JobResult(_)))
        .collect();
    assert_eq!(results.len(), 1);
    let r = result_of(&msgs);
    assert_eq!(r.state, JobState::Passed);
    assert_eq!(r.exit_code, Some(0));
    rig.settle().await;
    assert!(rig.pending().await.is_empty(), "acked result is not kept");
    assert_eq!(rig.process.runs().len(), 1);
}

#[tokio::test]
async fn timeout_reports_timeout_immediately() {
    let rig = Rig::new();
    tree(&rig);
    rig.script("job-1", JobScript::new(vec![StepScript::new(600 * SEC, 0)]));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("build", "make", 60)], 5),
    )
    .await;
    b.next_job_msg().await.unwrap();
    rig.advance(5 * SEC).await;
    let msgs = b.until_result().await;
    let r = result_of(&msgs);
    assert_eq!(r.state, JobState::Timeout);
    assert_eq!(
        rig.clock.now().0,
        START + 5_000,
        "no grace before the result"
    );
    assert_eq!(rig.process.runs()[0].timeout_s, 5);
    let step = msgs
        .iter()
        .find_map(|m| match m {
            RunnerToBroker::StepResult(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(step.state, StepState::Timeout);
}

#[tokio::test]
async fn runner_with_lower_ceiling_advertises_it() {
    let mut rig = Rig::new();
    rig.settings.max_job_timeout_s = Some(3600);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let caps = b.hello.capabilities.clone().unwrap();
    assert_eq!(caps.max_job_timeout_s, Some(3600));
    assert_eq!(b.hello.name, "runner-a");
    assert!(caps.platforms.contains("linux"));
}

#[tokio::test]
async fn job_above_the_ceiling_fails_instead_of_being_trimmed() {
    let mut rig = Rig::new();
    rig.settings.max_job_timeout_s = Some(3600);
    tree(&rig);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 60)], 7200),
    )
    .await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Failed);
    assert_eq!(
        r.detail.as_deref(),
        Some("job timeout 7200 s is above the runner ceiling 3600 s")
    );
    assert!(rig.process.runs().is_empty());
}

#[tokio::test]
async fn runner_restart_reports_runner_restarted() {
    let rig = Rig::new();
    tree(&rig);
    rig.script("job-1", JobScript::new(vec![StepScript::new(600 * SEC, 0)]));
    let (_c, h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 600)], 600),
    )
    .await;
    b.next_job_msg().await.unwrap();
    rig.settle().await;
    let root = format!("{FAKE_STEP_PID} 1");
    assert_eq!(
        rig.files.file(&state_dir().join("active-job")),
        Some(format!("job-1\n{root}").into_bytes()),
        "the marker names the root process of the running step"
    );
    h.abort();
    rig.settle().await;
    rig.net.cut(BROKER);
    let (_c2, _h2) = rig.start();
    let b2 = Broker::accept(&rig).await;
    assert_eq!(
        rig.process.kills(),
        vec![lr_abstractions::ProcessRef {
            pid: FAKE_STEP_PID,
            start: Some(1)
        }],
        "what the job left behind is killed at the start"
    );
    let r = result_of(&b2.until_result().await);
    assert_eq!(r.state, JobState::Failed);
    assert_eq!(r.reason, Some(ReasonCode::RunnerRestarted));
    assert_eq!(r.seq, 2, "the stream of the job continues after Started");
    assert_eq!(rig.process.runs().len(), 1, "the job is not run again");
}

#[tokio::test]
async fn heartbeat_every_15_seconds() {
    let rig = Rig::new();
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    rig.advance(14 * SEC).await;
    rig.advance(SEC).await;
    let m = b
        .until(|m| matches!(m, RunnerToBroker::Heartbeat { .. }))
        .await;
    assert_eq!(m, RunnerToBroker::Heartbeat { load: None });
    assert_eq!(rig.clock.now().0, START + 15_000);
}

#[tokio::test]
async fn heartbeat_carries_the_load_of_the_machine() {
    let rig = Rig::new();
    rig.load.set(Some(lr_abstractions::LoadSample {
        free_mem_mb: 2048,
        cpu_busy_percent: 40,
        cpu_cores: 8,
    }));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    rig.advance(15 * SEC).await;
    let m = b
        .until(|m| matches!(m, RunnerToBroker::Heartbeat { .. }))
        .await;
    assert_eq!(
        m,
        RunnerToBroker::Heartbeat {
            load: Some(lr_proto::WireLoad {
                free_mem_mb: 2048,
                cpu_busy_percent: 40,
                cpu_cores: 8,
            })
        }
    );
}

#[tokio::test]
async fn silent_broker_is_dropped_after_45_seconds_and_redialed() {
    let rig = Rig::new();
    let (_c, _h) = rig.start();
    let _b = Broker::accept(&rig).await;
    rig.advance(44 * SEC).await;
    assert_eq!(rig.net.connections(BROKER).len(), 1);
    rig.advance(SEC).await;
    rig.advance(SEC).await;
    let _b2 = Broker::accept(&rig).await;
    assert_eq!(rig.net.connections(BROKER).len(), 2);
}

#[tokio::test]
async fn cancel_sends_soft_stop_with_ten_second_grace() {
    let rig = Rig::new();
    tree(&rig);
    rig.script(
        "job-1",
        JobScript::new(vec![StepScript::new(600 * SEC, 0).ignores_soft_stop()]),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 600)], 600),
    )
    .await;
    b.next_job_msg().await.unwrap();
    b.cancel("job-1", 2, ReasonCode::CancelledByUser).await;
    rig.advance(9 * SEC).await;
    assert!(rig
        .pending()
        .await
        .iter()
        .all(|e| e.kind != lr_abstractions::OutboxKind::JobResult));
    rig.advance(SEC).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Cancelled);
    assert_eq!(r.reason, Some(ReasonCode::CancelledByUser));
    assert_eq!(rig.process.runs()[0].cancel_grace, DEFAULT_CANCEL_GRACE);
    assert_eq!(DEFAULT_CANCEL_GRACE, 10 * SEC);
}

#[tokio::test]
async fn repeated_cancel_is_dropped_and_gap_refused() {
    let rig = Rig::new();
    tree(&rig);
    rig.script("job-1", JobScript::new(vec![StepScript::new(600 * SEC, 0)]));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 600)], 600),
    )
    .await;
    b.next_job_msg().await.unwrap();
    b.cancel("job-1", 3, ReasonCode::CancelledByUser).await;
    let e = b.until(|m| matches!(m, RunnerToBroker::Error(_))).await;
    assert!(matches!(e, RunnerToBroker::Error(ref p) if p.code == ProtocolErrorCode::SeqGap));
    rig.advance(SEC).await;
    assert!(rig
        .pending()
        .await
        .iter()
        .all(|e| e.kind != lr_abstractions::OutboxKind::JobResult));
}

#[tokio::test]
async fn broker_lost_past_grace_kills_job_and_reports_broker_unavailable() {
    let rig = Rig::new();
    tree(&rig);
    rig.script(
        "job-1",
        JobScript::new(vec![StepScript::new(3600 * SEC, 0)]),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept_with(&rig, Some(180)).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 3600)], 3600),
    )
    .await;
    b.next_job_msg().await.unwrap();
    rig.net.set_reachable(BROKER, false);
    rig.net.cut(BROKER);
    for _ in 0..179 {
        rig.advance(SEC).await;
    }
    assert!(!has_result(&rig).await, "still running before 180 s");
    rig.advance(SEC).await;
    rig.advance(SEC).await;
    assert!(has_result(&rig).await);
    rig.net.set_reachable(BROKER, true);
    for _ in 0..40 {
        rig.advance(SEC).await;
        if rig.listener.pending() > 0 {
            break;
        }
    }
    let b2 = Broker::accept(&rig).await;
    let r = result_of(&b2.until_result().await);
    assert_eq!(r.state, JobState::Cancelled);
    assert_eq!(r.reason, Some(ReasonCode::BrokerUnavailable));
}

#[tokio::test]
async fn local_lost_kill_may_only_shorten_the_broker_value() {
    let mut rig = Rig::new();
    rig.settings.broker_lost_kill_s = Some(60);
    tree(&rig);
    rig.script(
        "job-1",
        JobScript::new(vec![StepScript::new(3600 * SEC, 0)]),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept_with(&rig, Some(180)).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 3600)], 3600),
    )
    .await;
    b.next_job_msg().await.unwrap();
    rig.net.set_reachable(BROKER, false);
    rig.net.cut(BROKER);
    for _ in 0..59 {
        rig.advance(SEC).await;
    }
    assert!(!has_result(&rig).await);
    rig.advance(SEC).await;
    rig.advance(SEC).await;
    assert!(has_result(&rig).await, "killed after the shorter 60 s");
}

#[tokio::test]
async fn settings_update_changes_the_lost_kill_time() {
    let rig = Rig::new();
    tree(&rig);
    rig.script(
        "job-1",
        JobScript::new(vec![StepScript::new(3600 * SEC, 0)]),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept_with(&rig, Some(600)).await;
    b.send(BrokerToRunner::SettingsUpdate(SettingsUpdate {
        broker_lost_kill_s: Some(30),
    }))
    .await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 3600)], 3600),
    )
    .await;
    b.next_job_msg().await.unwrap();
    rig.net.set_reachable(BROKER, false);
    rig.net.cut(BROKER);
    for _ in 0..29 {
        rig.advance(SEC).await;
    }
    assert!(!has_result(&rig).await);
    rig.advance(SEC).await;
    rig.advance(SEC).await;
    assert!(has_result(&rig).await);
}

#[tokio::test]
async fn authorization_refusal_kills_at_once_clears_outbox_and_stops() {
    let rig = Rig::new();
    tree(&rig);
    rig.script(
        "job-1",
        JobScript::new(vec![StepScript::new(600 * SEC, 0).ignores_soft_stop()]),
    );
    let (_c, h) = rig.start();
    let mut b = Broker::accept(&rig).await;
    b.auto_ack = false;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 600)], 600),
    )
    .await;
    b.next_job_msg().await.unwrap();
    b.send(BrokerToRunner::Error(ProtocolError {
        code: ProtocolErrorCode::KeyRevoked,
        message: "runner removed".into(),
        supported: None,
    }))
    .await;
    rig.settle().await;
    rig.settle().await;
    let exit = h.await.unwrap();
    assert!(matches!(exit, AgentExit::Refused(ref e) if e.code == ProtocolErrorCode::KeyRevoked));
    assert_eq!(rig.clock.now().0, START, "killed without the grace");
    assert!(
        rig.pending().await.is_empty(),
        "outbox dropped when removed"
    );
}

#[tokio::test]
async fn repeated_assign_runs_once() {
    let rig = Rig::new();
    tree(&rig);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let spec = worktree_spec("/w/tree", vec![step("b", "make", 60)], 60);
    b.assign("job-1", 1, spec.clone()).await;
    b.assign("job-1", 1, spec.clone()).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Passed);
    b.assign("job-1", 2, spec).await;
    rig.advance(SEC).await;
    assert_eq!(
        rig.process.runs().len(),
        1,
        "a job already run is never run twice"
    );
}

#[tokio::test]
async fn assign_with_a_seq_gap_is_refused_and_not_run() {
    let rig = Rig::new();
    tree(&rig);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        2,
        worktree_spec("/w/tree", vec![step("b", "make", 60)], 60),
    )
    .await;
    let e = b.until(|m| matches!(m, RunnerToBroker::Error(_))).await;
    assert!(matches!(e, RunnerToBroker::Error(ref p) if p.code == ProtocolErrorCode::SeqGap));
    rig.settle().await;
    assert!(rig.process.runs().is_empty());
}

#[tokio::test]
async fn busy_runner_fails_a_second_assignment() {
    let rig = Rig::new();
    tree(&rig);
    rig.script("job-1", JobScript::new(vec![StepScript::new(600 * SEC, 0)]));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let spec = worktree_spec("/w/tree", vec![step("b", "make", 600)], 600);
    b.assign("job-1", 1, spec.clone()).await;
    b.next_job_msg().await.unwrap();
    b.assign("job-2", 1, spec).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.job, JobId("job-2".into()));
    assert_eq!(r.state, JobState::Failed);
    assert_eq!(r.detail.as_deref(), Some("runner is busy with job job-1"));
    assert_eq!(rig.process.runs().len(), 1);
}

#[tokio::test]
async fn missing_work_tree_fails_the_job() {
    let rig = Rig::new();
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/none", vec![step("b", "make", 60)], 60),
    )
    .await;
    let msgs = b.until_result().await;
    let r = result_of(&msgs);
    assert_eq!(r.state, JobState::Failed);
    assert!(r.detail.unwrap().contains("work tree"));
    let skipped = msgs
        .iter()
        .filter(|m| matches!(m, RunnerToBroker::StepResult(s) if s.state == StepState::Skipped))
        .count();
    assert_eq!(skipped, 1);
}
