mod common;

use common::*;
use lr_abstractions::{CacheKey, Frame, Listener, RunnerStore};
use lr_core::{JobId, Timestamp};
use lr_proto::{
    decode, BrokerToRunner, ProtocolError, ProtocolErrorCode, Resume, ResumePoint, RunnerToBroker,
};
use lr_runner::{AgentExit, Retention, WorkspaceLayout, WORKSPACE_CLIENT};
use std::path::Path;
use std::time::Duration;

const SEC: Duration = Duration::from_secs(1);
const DAY: u64 = 86_400_000;

#[tokio::test]
async fn hello_refused_for_authorization_stops_the_agent() {
    let rig = Rig::new();
    let (_c, h) = rig.start();
    let t = rig.listener.accept().await.unwrap();
    let first = decode::<RunnerToBroker>(t.recv().await.unwrap().as_bytes()).unwrap();
    assert!(matches!(first, RunnerToBroker::Hello(_)));
    t.send(
        Frame::json(&BrokerToRunner::Error(ProtocolError {
            code: ProtocolErrorCode::Unauthorized,
            message: "unknown or revoked key".into(),
            supported: None,
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    rig.settle().await;
    let exit = h.await.unwrap();
    assert!(matches!(exit, AgentExit::Refused(ref e) if e.code == ProtocolErrorCode::Unauthorized));
}

#[tokio::test]
async fn other_hello_refusal_is_retried() {
    let rig = Rig::new();
    let (_c, _h) = rig.start();
    let t = rig.listener.accept().await.unwrap();
    t.recv().await.unwrap();
    t.send(
        Frame::json(&BrokerToRunner::Error(ProtocolError {
            code: ProtocolErrorCode::Unavailable,
            message: "storage".into(),
            supported: None,
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    rig.advance(SEC).await;
    let _b = Broker::accept(&rig).await;
}

#[tokio::test]
async fn malformed_broker_message_is_refused_and_the_session_kept() {
    let rig = Rig::new();
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.t.send(Frame::from_bytes(
        b"{\"type\":\"assign\",\"job\":1}".to_vec(),
    ))
    .await
    .unwrap();
    let e = b.until(|m| matches!(m, RunnerToBroker::Error(_))).await;
    assert!(matches!(e, RunnerToBroker::Error(ref p) if p.code == ProtocolErrorCode::Malformed));
    b.t.send(Frame::from_bytes(b"{\"type\":\"future_thing\"}".to_vec()))
        .await
        .unwrap();
    rig.advance(15 * SEC).await;
    b.until(|m| matches!(m, RunnerToBroker::Heartbeat)).await;
}

#[tokio::test]
async fn broker_resume_drops_what_it_already_has() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    let (_c, _h) = rig.start();
    let mut b = Broker::accept(&rig).await;
    b.auto_ack = false;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 60)], 60),
    )
    .await;
    b.until_result().await;
    rig.settle().await;
    assert_eq!(rig.pending().await.len(), 3);
    b.send(BrokerToRunner::Resume(Resume {
        points: vec![ResumePoint {
            job: JobId("job-1".into()),
            last_seq: 2,
        }],
    }))
    .await;
    rig.settle().await;
    assert_eq!(rig.pending().await.len(), 1);
}

#[tokio::test]
async fn reconnect_backoff_doubles_up_to_thirty_seconds() {
    let rig = Rig::new();
    rig.net.set_reachable(BROKER, false);
    let (_c, _h) = rig.start();
    for _ in 0..92 {
        rig.advance(SEC).await;
    }
    rig.net.set_reachable(BROKER, true);
    let mut waited = 0;
    while rig.listener.pending() == 0 {
        rig.advance(SEC).await;
        waited += 1;
        assert!(waited <= 30, "next attempt at most 30 s away");
    }
    assert_eq!(waited, 29, "attempts at 0, 1, 3, 7, 15, 31, 61, 91, 121 s");
}

#[tokio::test]
async fn mirror_is_indexed_and_expires_with_workspace_retention() {
    let url = "https://git.example.com/acme/app.git";
    let mut rig = Rig::new();
    rig.settings.workspace = Retention {
        max_size_gb: None,
        retention_days: Some(7),
    };
    let mirror = WorkspaceLayout::new(state_dir()).mirror_dir("acme/app", url);
    {
        let (_c, h) = rig.start();
        let b = Broker::accept(&rig).await;
        b.assign("job-1", 1, commit_spec(vec![step("b", "make", 60)], 60))
            .await;
        b.until_result().await;
        rig.settle().await;
        h.abort();
        rig.settle().await;
    }
    let entries = rig.store.cache_entries().await.unwrap();
    let entry = entries
        .iter()
        .find(|e| e.key.client == WORKSPACE_CLIENT)
        .expect("the mirror is in the index");
    assert_eq!(entry.path, mirror.display().to_string());
    assert_eq!(entry.last_used, Timestamp(START));
    rig.files.mkdir(&mirror);
    rig.clock.advance(Duration::from_millis(8 * DAY));
    rig.net.cut(BROKER);
    let (_c, _h) = rig.start();
    let _b = Broker::accept(&rig).await;
    assert!(
        !rig.files.is_dir(&mirror),
        "unused for 8 days, removed at start"
    );
    let left: Vec<CacheKey> = rig
        .store
        .cache_entries()
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.key)
        .collect();
    assert!(left.iter().all(|k| k.client != WORKSPACE_CLIENT));
}

#[tokio::test]
async fn runner_never_listens() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script(
        "job-1",
        lr_abstractions::fakes::JobScript::new(vec![lr_abstractions::fakes::StepScript::new(
            SEC, 0,
        )]),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("build", "make", 60)], 60),
    )
    .await;
    b.next_job_msg().await.unwrap();
    rig.advance(SEC).await;
    let msgs = b.until_result().await;
    assert_eq!(result_of(&msgs).state, lr_core::JobState::Passed);
    assert_eq!(
        rig.net.listening(),
        vec![BROKER.to_string()],
        "the only bound address is the broker's: the runner dials out and opens no port"
    );
    assert_eq!(rig.net.connections(BROKER).len(), 1);
}
