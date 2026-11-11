mod common;

use common::harness::*;
use common::listeners::Stall;
use lr_abstractions::{OutboxEntry, OutboxKind};
use lr_core::{JobId, Timestamp};
use lr_proto::*;

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

async fn until(what: &str, probe: impl Fn() -> bool) {
    for _ in 0..5_000_000 {
        if probe() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("never happened: {what}");
}

async fn engine_caught_up(h: &Harness) {
    h.handle.revoked("nobody-at-all").await;
}

async fn read_requests(h: &Harness, hub: &Conn, stall: &Stall, replies: u64) {
    let before = stall.reads_started();
    for req in 0..replies {
        hub.send(&HubToBroker::Read(ReadCall {
            req,
            request: ReadRequest::Runners,
        }))
        .await;
    }
    let all = before + replies as usize;
    until(
        "every request is handed to the engine or the hub is cut off",
        || stall.reads_started() >= all || stall.closes() > 0,
    )
    .await;
    engine_caught_up(h).await;
}

async fn stalled_replay(let_through: usize) -> (Harness, Stall, Conn) {
    let (h, stall) = Harness::stalling("[limits]\napi_requests_per_min = 100000\n");
    pending_chunks(&h, 5000).await;
    let hub = h.local_hub("alex").await;
    let sent_before = stall.sent();
    stall.start();
    let reads = stall.reads_started();
    hub.send(&resume()).await;
    until("the resume is handed to the engine", || {
        stall.reads_started() > reads
    })
    .await;
    engine_caught_up(&h).await;
    stall.let_through(let_through);
    until("the allowed frames are written", || {
        stall.sent() == sent_before + let_through
    })
    .await;
    engine_caught_up(&h).await;
    (h, stall, hub)
}

async fn stalled_replay_then_replies(let_through: usize, replies: u64) -> (Harness, Stall, Conn) {
    let (h, stall, hub) = stalled_replay(let_through).await;
    read_requests(&h, &hub, &stall, replies).await;
    (h, stall, hub)
}

fn slow_disconnects(h: &Harness, why: &str) -> usize {
    h.log
        .records()
        .iter()
        .filter(|r| r.message == format!("slow peer disconnected: {why}"))
        .count()
}

const FULL: &str = "its outgoing queue is full";

#[tokio::test]
async fn a_replay_keeps_at_most_256_frames_queued_so_live_replies_fit_the_rest() {
    let (h, stall, _hub) = stalled_replay_then_replies(0, 1024 - 255).await;
    assert_eq!(slow_disconnects(&h, FULL), 0);
    assert_eq!(stall.closes(), 0);
}

#[tokio::test]
async fn one_live_reply_past_the_free_room_disconnects_the_hub() {
    let (h, _stall, _hub) = stalled_replay_then_replies(0, 1024 - 255 + 1).await;
    assert_eq!(slow_disconnects(&h, FULL), 1);
}

#[tokio::test]
async fn no_next_portion_while_more_than_half_of_the_window_is_queued() {
    let (h, _stall, _hub) = stalled_replay_then_replies(100, 1024 - 255 + 100).await;
    assert_eq!(
        slow_disconnects(&h, FULL),
        0,
        "the 100 sent frames were not refilled"
    );
    let (h, _stall, _hub) = stalled_replay_then_replies(100, 1024 - 255 + 101).await;
    assert_eq!(slow_disconnects(&h, FULL), 1);
}

#[tokio::test]
async fn the_next_portion_comes_once_the_queue_is_down_to_half() {
    let (h, _stall, _hub) = stalled_replay_then_replies(200, 1024 - 255 + 128).await;
    assert_eq!(
        slow_disconnects(&h, FULL),
        1,
        "the queue was topped up at 128 frames, so its free room shrank again"
    );
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
