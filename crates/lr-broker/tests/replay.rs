mod common;

use common::harness::*;
use lr_abstractions::{BrokerStore, OutboxKind};
use lr_core::{JobId, Timestamp};
use lr_proto::*;
use std::sync::atomic::Ordering;
use std::time::Duration;

const ALEX: &str = "local:alex";
const PENDING: u64 = 5000;

async fn pending_chunks(h: &Harness, n: u64) {
    for i in 0..n {
        let msg = BrokerToHub::Chunk(Chunk {
            job: JobId("j1".into()),
            seq: 0,
            step: Some(0),
            text: format!("line {i}\n"),
        });
        h.store
            .push_result(
                ALEX,
                &JobId("j1".into()),
                OutboxKind::Chunk,
                serde_json::to_string(&msg).unwrap(),
                Timestamp(START),
                u64::MAX,
            )
            .await
            .unwrap();
    }
}

fn resume_from(last_seq: u64) -> HubToBroker {
    HubToBroker::Resume(Resume {
        points: vec![ResumePoint {
            job: JobId("j1".into()),
            last_seq,
        }],
    })
}

#[tokio::test]
async fn resume_with_thousands_of_pending_entries_delivers_all_in_order() {
    let h = Harness::start("");
    pending_chunks(&h, PENDING).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&resume_from(0)).await;
    let mut seqs = Vec::new();
    while seqs.len() < PENDING as usize {
        match hub.hub().await {
            BrokerToHub::Chunk(c) => seqs.push(c.seq),
            other => panic!("expected a chunk, got {other:?}"),
        }
    }
    assert_eq!(seqs, (1..=PENDING).collect::<Vec<_>>());
    hub.sync().await;
}

#[tokio::test]
async fn live_results_wait_behind_the_replay() {
    let h = Harness::start("");
    pending_chunks(&h, PENDING).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&resume_from(0)).await;
    hub.send(&HubToBroker::Read(ReadCall {
        req: 1,
        request: ReadRequest::Log {
            job: JobId("nope".into()),
            step: None,
            tail: None,
            follow: false,
            client: None,
        },
    }))
    .await;
    let mut last = 0;
    let mut count = 0;
    while count < PENDING {
        if let BrokerToHub::Chunk(c) = hub.hub().await {
            assert_eq!(c.seq, last + 1);
            last = c.seq;
            count += 1;
        }
    }
}

#[tokio::test]
async fn hub_that_never_reads_is_still_disconnected() {
    let (h, stall) = Harness::stalling("");
    pending_chunks(&h, PENDING).await;
    let hub = h.local_hub("alex").await;
    stall.start();
    hub.send(&resume_from(0)).await;
    for req in 0..2000u64 {
        hub.send(&HubToBroker::Read(ReadCall {
            req,
            request: ReadRequest::Runners,
        }))
        .await;
    }
    for _ in 0..500 {
        if stall.closes() >= 1 {
            stall.on.store(false, Ordering::SeqCst);
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("a hub that never reads was not disconnected");
}
