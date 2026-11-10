mod common;

use common::harness::*;
use lr_core::{JobId, JobState};
use lr_proto::*;

fn follow(job: &str) -> HubToBroker {
    HubToBroker::Read(ReadCall {
        req: 77,
        request: ReadRequest::Log {
            job: JobId(job.into()),
            step: None,
            tail: None,
            follow: true,
            client: None,
        },
    })
}

async fn log_chunk(c: &mut Conn) -> LogChunk {
    loop {
        if let BrokerToHub::LogChunk(l) = c.hub().await {
            return l;
        }
    }
}

async fn running(h: &Harness) -> (Conn, Conn) {
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    r.send(&chunk("j1", 2, "a\n")).await;
    r.acked(2).await;
    (r, hub)
}

#[tokio::test]
async fn log_follow_streams_new_chunks_until_the_job_ends() {
    let h = Harness::start("");
    let (mut r, _hub) = running(&h).await;
    let mut viewer = h.local_hub("alex").await;
    viewer.send(&follow("j1")).await;
    let first = log_chunk(&mut viewer).await;
    assert_eq!(
        (first.req, first.text.as_str(), first.end),
        (77, "a\n", false)
    );
    r.send(&chunk("j1", 3, "b\n")).await;
    r.acked(3).await;
    let next = log_chunk(&mut viewer).await;
    assert_eq!((next.text.as_str(), next.end), ("b\n", false));
    r.send(&result("j1", 4, JobState::Passed, 0)).await;
    let last = log_chunk(&mut viewer).await;
    assert_eq!((last.req, last.text.as_str(), last.end), (77, "", true));
    r.acked(4).await;
    r.send(&chunk("j1", 5, "late\n")).await;
    r.acked(5).await;
    let after = viewer.sync().await;
    assert!(!after.iter().any(|m| matches!(m, BrokerToHub::LogChunk(_))));
}

#[tokio::test]
async fn log_follow_of_a_finished_job_returns_the_log_and_ends() {
    let h = Harness::start("");
    let (mut r, _hub) = running(&h).await;
    r.send(&result("j1", 3, JobState::Passed, 0)).await;
    r.acked(3).await;
    let mut viewer = h.local_hub("alex").await;
    viewer.send(&follow("j1")).await;
    let only = log_chunk(&mut viewer).await;
    assert_eq!((only.text.as_str(), only.end), ("a\n", true));
}

#[tokio::test]
async fn log_follow_is_refused_to_another_client() {
    let h = Harness::start("");
    let (_r, _hub) = running(&h).await;
    let mut bob = h.local_hub("bob").await;
    bob.send(&follow("j1")).await;
    let reply = loop {
        if let BrokerToHub::ReadReply(a) = bob.hub().await {
            break a.reply;
        }
    };
    assert!(matches!(
        reply,
        ReadReply::NotFound { .. } | ReadReply::Denied { .. }
    ));
}

#[tokio::test]
async fn a_follow_takes_a_subscription_slot() {
    let h = Harness::start("");
    let (_r, _hub) = running(&h).await;
    let mut viewer = h.local_hub("alex").await;
    for _ in 0..lr_broker::MAX_SUBSCRIPTIONS {
        let reply = viewer
            .read(ReadRequest::Subscribe {
                topics: vec![Topic::Runners],
            })
            .await;
        assert!(matches!(reply, ReadReply::Subscribed { .. }));
    }
    viewer.send(&follow("j1")).await;
    let reply = loop {
        if let BrokerToHub::ReadReply(a) = viewer.hub().await {
            break a.reply;
        }
    };
    assert!(matches!(reply, ReadReply::Failed { .. }), "{reply:?}");
}
