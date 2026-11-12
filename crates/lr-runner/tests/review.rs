mod common;

use common::*;
use lr_abstractions::fakes::{JobScript, StepScript};
use lr_abstractions::{CacheEntry, CacheKey, Clock, Frame, Listener, OutboxKind, RunnerStore};
use lr_core::{JobId, JobState, Timestamp};
use lr_proto::{
    decode, BrokerToRunner, Chunk, RunnerToBroker, SettingsUpdate, Welcome, PROTOCOL_VERSION,
};
use lr_runner::{Retention, WorkspaceLayout, WORKSPACE_CLIENT};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

const SEC: Duration = Duration::from_secs(1);
const MS: Duration = Duration::from_millis(1);
const DAY: u64 = 86_400_000;
const URL: &str = "https://git.example.com/acme/app.git";

async fn has_result(rig: &Rig) -> bool {
    rig.pending()
        .await
        .iter()
        .any(|e| e.kind == OutboxKind::JobResult)
}

async fn running_job(rig: &Rig, lost_kill: Option<u64>) -> Broker {
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script(
        "job-1",
        JobScript::new(vec![StepScript::new(Duration::from_secs(864_000), 0)]),
    );
    let b = Broker::accept_with(rig, lost_kill).await;
    b.assign(
        "job-1",
        1,
        worktree_spec(
            "/w/tree",
            vec![step("b", "make", u64::MAX / 2000)],
            u64::MAX / 2000,
        ),
    )
    .await;
    b.next_job_msg().await.unwrap();
    rig.settle().await;
    b
}

async fn expect_kill_at(rig: &Rig, after: Duration) {
    rig.clock.advance(after - MS);
    rig.settle().await;
    assert!(!has_result(rig).await, "not killed before {after:?}");
    rig.clock.advance(MS);
    rig.settle().await;
    rig.settle().await;
    assert!(has_result(rig).await, "killed at exactly {after:?}");
}

#[tokio::test]
async fn huge_welcome_values_are_clamped_without_panic() {
    let rig = Rig::new();
    let (_c, _h) = rig.start();
    let t = rig.listener.accept().await.unwrap();
    t.recv().await.unwrap();
    t.send(
        Frame::json(&BrokerToRunner::Welcome(Welcome {
            version: PROTOCOL_VERSION,
            zone: None,
            heartbeat_s: u64::MAX,
            broker_lost_kill_s: Some(u64::MAX),
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    rig.settle().await;
    let sent = rig.gate.sent();
    rig.clock.advance(3599 * SEC);
    rig.settle().await;
    assert_eq!(rig.gate.sent(), sent, "no heartbeat before 3600 s");
    rig.clock.advance(SEC);
    rig.settle().await;
    loop {
        let m = decode::<RunnerToBroker>(t.recv().await.unwrap().as_bytes()).unwrap();
        if matches!(m, RunnerToBroker::Heartbeat { .. }) {
            break;
        }
    }
    assert_eq!(rig.clock.now().0, START + 3_600_000);
}

#[tokio::test]
async fn huge_lost_kill_is_clamped_to_one_day() {
    let rig = Rig::new();
    let (_c, _h) = rig.start();
    let _b = running_job(&rig, Some(u64::MAX)).await;
    rig.net.set_reachable(BROKER, false);
    rig.net.cut(BROKER);
    rig.settle().await;
    expect_kill_at(&rig, Duration::from_secs(86_400)).await;
}

#[tokio::test]
async fn huge_settings_update_is_clamped_and_local_value_still_shortens() {
    let mut rig = Rig::new();
    rig.settings.broker_lost_kill_s = Some(100);
    let (_c, _h) = rig.start();
    let b = running_job(&rig, Some(30)).await;
    b.send(BrokerToRunner::SettingsUpdate(SettingsUpdate {
        broker_lost_kill_s: Some(u64::MAX),
    }))
    .await;
    rig.settle().await;
    rig.net.set_reachable(BROKER, false);
    rig.net.cut(BROKER);
    rig.settle().await;
    expect_kill_at(&rig, Duration::from_secs(100)).await;
}

#[tokio::test]
async fn lost_kill_fires_while_a_dial_hangs() {
    let rig = Rig::new();
    let (_c, _h) = rig.start();
    let _b = running_job(&rig, Some(180)).await;
    rig.gate.hang_dials(true);
    rig.net.cut(BROKER);
    rig.settle().await;
    expect_kill_at(&rig, 180 * SEC).await;
}

#[tokio::test]
async fn lost_kill_fires_while_the_welcome_never_comes() {
    let rig = Rig::new();
    let (_c, _h) = rig.start();
    let _b = running_job(&rig, Some(180)).await;
    rig.net.cut(BROKER);
    rig.settle().await;
    expect_kill_at(&rig, 180 * SEC).await;
}

#[tokio::test]
async fn marker_failure_fails_the_job_before_anything_runs() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    rig.files.fail(Some("disk full"));
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 60)], 60),
    )
    .await;
    let msgs = b.until_result().await;
    let r = result_of(&msgs);
    assert_eq!(r.state, JobState::Failed);
    assert_eq!(
        r.detail.as_deref(),
        Some("active job marker not written: file system: disk full")
    );
    assert!(!msgs.iter().any(|m| matches!(m, RunnerToBroker::Started(_))));
    assert!(rig.process.runs().is_empty());
}

#[tokio::test]
async fn marker_exists_when_the_assign_is_acked() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script("job-1", JobScript::new(vec![StepScript::new(600 * SEC, 0)]));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 600)], 600),
    )
    .await;
    b.until(|m| matches!(m, RunnerToBroker::Ack(_))).await;
    let marker = rig.files.file(&state_dir().join("active-job")).unwrap();
    assert!(marker.starts_with(b"job-1"), "{marker:?}");
}

#[tokio::test]
async fn sweep_before_a_job_keeps_the_mirror_it_uses() {
    let mut rig = Rig::new();
    rig.settings.workspace = Retention {
        max_size_gb: None,
        retention_days: Some(7),
    };
    let layout = WorkspaceLayout::new(state_dir());
    let mirror = layout.mirror_dir("acme/app", URL);
    rig.files.mkdir(&mirror);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let ws = layout.project_dir("acme/app", URL);
    rig.store
        .upsert_cache(CacheEntry {
            key: CacheKey {
                client: WORKSPACE_CLIENT.into(),
                project: ws.file_name().unwrap().to_string_lossy().into_owned(),
                name: "mirror".into(),
            },
            path: mirror.display().to_string(),
            bytes: 0,
            last_used: Timestamp(START - 8 * DAY),
        })
        .await
        .unwrap();
    b.assign("job-1", 1, commit_spec(vec![step("b", "make", 60)], 60))
        .await;
    b.until_result().await;
    let cmds: Vec<String> = rig.process.runs()[0]
        .steps
        .iter()
        .map(|s| s.spec.command.clone())
        .collect();
    assert!(cmds.iter().all(|c| !c.contains("clone")), "{cmds:?}");
}

fn cache_entry(client: &str, name: &str, last_used: u64) -> CacheEntry {
    let key = CacheKey {
        client: client.into(),
        project: "acme/app".into(),
        name: name.into(),
    };
    CacheEntry {
        path: lr_runner::CacheLayout::new(state_dir())
            .path(&key)
            .display()
            .to_string(),
        key,
        bytes: 1,
        last_used: Timestamp(last_used),
    }
}

#[tokio::test]
async fn hourly_sweep_during_a_job_spares_its_caches_only() {
    let mut rig = Rig::new();
    rig.settings.cache = Retention {
        max_size_gb: None,
        retention_days: Some(14),
    };
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script(
        "job-1",
        JobScript::new(vec![StepScript::new(7200 * SEC, 0)]),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let mut spec = worktree_spec("/w/tree", vec![step("b", "make", 7200)], 7200);
    spec.requirements.cache = vec!["deps".into()];
    b.assign("job-1", 1, spec).await;
    b.next_job_msg().await.unwrap();
    rig.settle().await;
    let old = START - 15 * DAY;
    rig.store
        .upsert_cache(cache_entry("client-1", "deps", old))
        .await
        .unwrap();
    rig.store
        .upsert_cache(cache_entry("client-9", "other", old))
        .await
        .unwrap();
    for _ in 0..120 {
        rig.advance(30 * SEC).await;
        let _ =
            b.t.send(Frame::json(&BrokerToRunner::Heartbeat).unwrap())
                .await;
    }
    let names: Vec<String> = rig
        .store
        .cache_entries()
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.key.name)
        .collect();
    assert!(!has_result(&rig).await, "the job still runs");
    assert_eq!(names, vec!["deps".to_string()]);
}

#[tokio::test]
async fn unreadable_stored_message_is_replaced_not_skipped() {
    let rig = Rig::new();
    let job = JobId("job-1".into());
    rig.store
        .push_outbox(&job, OutboxKind::Chunk, "not json".into(), Timestamp(START))
        .await
        .unwrap();
    let good = RunnerToBroker::Chunk(Chunk {
        job: job.clone(),
        seq: 0,
        step: None,
        text: "fine".into(),
    });
    rig.store
        .push_outbox(
            &job,
            OutboxKind::Chunk,
            serde_json::to_string(&good).unwrap(),
            Timestamp(START),
        )
        .await
        .unwrap();
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let first = b.next_job_msg().await.unwrap();
    let second = b.next_job_msg().await.unwrap();
    let RunnerToBroker::Chunk(c1) = first else {
        panic!("placeholder chunk expected")
    };
    assert_eq!(c1.seq, 1);
    assert_eq!(c1.text, "[localrunner: stored message 1 is unreadable]\n");
    assert!(matches!(second, RunnerToBroker::Chunk(ref c) if c.seq == 2 && c.text == "fine"));
}

#[tokio::test]
async fn stalled_broker_is_dropped_and_a_large_outbox_replayed() {
    let rig = Rig::new();
    let job = JobId("job-1".into());
    for _ in 0..5000 {
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
    let (_c, _h) = rig.start();
    let t = rig.listener.accept().await.unwrap();
    t.recv().await.unwrap();
    rig.gate.stall_sends(true);
    t.send(
        Frame::json(&BrokerToRunner::Welcome(Welcome {
            version: PROTOCOL_VERSION,
            zone: None,
            heartbeat_s: 15,
            broker_lost_kill_s: None,
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    rig.advance(44 * SEC).await;
    assert_eq!(rig.net.connections(BROKER).len(), 1);
    rig.gate.stall_sends(false);
    rig.advance(SEC).await;
    rig.advance(SEC).await;
    let mut b = Broker::accept(&rig).await;
    b.auto_ack = false;
    let mut chunks = 0;
    while chunks < 5000 {
        if matches!(b.next_job_msg().await.unwrap(), RunnerToBroker::Chunk(_)) {
            chunks += 1;
        }
    }
    assert!(
        rig.outbox_reads() < 20,
        "outbox read {} times",
        rig.outbox_reads()
    );
}

#[tokio::test]
async fn new_entries_are_sent_without_rereading_the_outbox() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    let steps: Vec<_> = (0..300).map(|i| step(&format!("s{i}"), "x", 10)).collect();
    rig.script(
        "job-1",
        JobScript::new(
            (0..300)
                .map(|_| StepScript::new(SEC, 0).output(b"o\n"))
                .collect(),
        ),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, worktree_spec("/w/tree", steps, 3600))
        .await;
    let reader = tokio::spawn(async move { b.until_result().await });
    for _ in 0..300 {
        rig.advance(SEC).await;
    }
    let msgs = reader.await.unwrap();
    assert_eq!(result_of(&msgs).state, JobState::Passed);
    assert!(
        rig.outbox_reads() < 20,
        "outbox read {} times",
        rig.outbox_reads()
    );
}

#[tokio::test]
async fn secret_mapping_to_a_reserved_variable_fails_the_job() {
    for name in ["path", "Home", "tmp", "localrunner_job_id"] {
        let rig = Rig::new();
        rig.files.mkdir(Path::new("/w/tree"));
        rig.secrets.insert(name, "v");
        let (_c, _h) = rig.start();
        let b = Broker::accept(&rig).await;
        let mut spec = worktree_spec("/w/tree", vec![step("b", "make", 60)], 60);
        spec.secrets = vec![name.into()];
        b.assign("job-1", 1, spec).await;
        let r = result_of(&b.until_result().await);
        assert_eq!(r.state, JobState::Failed);
        let var = name.to_ascii_uppercase();
        assert_eq!(
            r.detail,
            Some(format!(
                "secret {name} would replace the reserved variable {var}"
            ))
        );
        assert!(rig.process.runs().is_empty());
    }
}

#[tokio::test]
async fn refusal_with_a_failing_store_does_not_block_the_session() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script("job-1", JobScript::new(vec![StepScript::new(600 * SEC, 0)]));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let spec = worktree_spec("/w/tree", vec![step("b", "make", 600)], 600);
    b.assign("job-1", 1, spec.clone()).await;
    b.next_job_msg().await.unwrap();
    rig.fail_push.store(true, Ordering::SeqCst);
    b.assign("job-2", 1, spec).await;
    rig.settle().await;
    rig.advance(15 * SEC).await;
    b.until(|m| matches!(m, RunnerToBroker::Heartbeat { .. }))
        .await;
    assert!(rig.logs_text().contains("job result not stored"));
}

#[tokio::test]
async fn profile_steps_get_the_remaining_time_rounded_up() {
    let rig = Rig::new();
    rig.script(
        "job-1",
        JobScript::new(vec![StepScript::new(Duration::from_millis(59_100), 0)]),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, commit_spec(vec![step("b", "make", 60)], 60))
        .await;
    rig.advance(Duration::from_millis(59_100)).await;
    rig.advance(SEC).await;
    b.until_result().await;
    assert_eq!(rig.process.runs()[1].timeout_s, 1);
}
