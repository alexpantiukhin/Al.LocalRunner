mod common;

use common::harness::*;
use common::listeners::Stall;
use lr_abstractions::{OutboxEntry, OutboxKind};
use lr_core::{JobId, Timestamp};
use lr_proto::*;
use std::time::Duration;

async fn pending_chunks(h: &Harness, n: u64) {
    let msg = BrokerToHub::Chunk(Chunk {
        job: JobId("j1".into()),
        seq: 0,
        step: Some(0),
        text: "line\n".into(),
    });
    let payload = serde_json::to_string(&msg).unwrap();
    *h.faults.pending.lock().unwrap() = (1..=n)
        .map(|seq| OutboxEntry {
            job: JobId("j1".into()),
            seq,
            kind: OutboxKind::Chunk,
            payload: payload.clone(),
            created_at: Timestamp(START),
        })
        .collect();
}

fn resume() -> HubToBroker {
    HubToBroker::Resume(Resume {
        points: vec![ResumePoint {
            job: JobId("j1".into()),
            last_seq: 0,
        }],
    })
}

async fn settle() {
    for _ in 0..2000 {
        tokio::task::yield_now().await;
    }
}

async fn stalled_replay_then_replies(replies: u64) -> (Harness, Stall) {
    let (h, stall) = Harness::stalling("[limits]\napi_requests_per_min = 100000\n");
    pending_chunks(&h, 5000).await;
    let hub = h.local_hub("alex").await;
    stall.start();
    hub.send(&resume()).await;
    settle().await;
    for req in 0..replies {
        hub.send(&HubToBroker::Read(ReadCall {
            req,
            request: ReadRequest::Runners,
        }))
        .await;
    }
    settle().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    settle().await;
    std::mem::forget(hub);
    (h, stall)
}

fn slow_disconnects(h: &Harness, why: &str) -> usize {
    h.log
        .records()
        .iter()
        .filter(|r| r.message == format!("slow peer disconnected: {why}"))
        .count()
}

#[tokio::test]
async fn a_replay_keeps_at_most_256_frames_queued_so_live_replies_fit_the_rest() {
    let (h, stall) = stalled_replay_then_replies(1024 - 255).await;
    assert_eq!(slow_disconnects(&h, "its outgoing queue is full"), 0);
    assert_eq!(stall.closes(), 0);
}

#[tokio::test]
async fn one_live_reply_past_the_free_room_disconnects_the_hub() {
    let (h, _stall) = stalled_replay_then_replies(1024 - 255 + 1).await;
    assert_eq!(slow_disconnects(&h, "its outgoing queue is full"), 1);
}

#[tokio::test]
async fn more_than_100000_pending_frames_make_the_hub_slow() {
    let h = Harness::start("");
    pending_chunks(&h, 100_001).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&resume()).await;
    assert!(hub.closed().await, "the hub stays connected");
    assert_eq!(slow_disconnects(&h, "its replay backlog is full"), 1);
}

#[tokio::test]
async fn exactly_100000_pending_frames_are_replayed() {
    let h = Harness::start("");
    pending_chunks(&h, 100_000).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&resume()).await;
    match hub.hub().await {
        BrokerToHub::Chunk(c) => assert_eq!(c.seq, 1),
        other => panic!("expected the first chunk, got {other:?}"),
    }
    assert_eq!(slow_disconnects(&h, "its replay backlog is full"), 0);
}
