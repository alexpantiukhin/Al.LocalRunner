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
    assert_eq!(
        reply,
        ReadReply::Denied {
            reason: "полный лог отдаётся только клиенту-владельцу запуска".into()
        }
    );
}

fn follow_step(job: &str, step: &str) -> HubToBroker {
    let HubToBroker::Read(mut call) = follow(job) else {
        unreachable!()
    };
    if let ReadRequest::Log { step: s, .. } = &mut call.request {
        *s = Some(step.into());
    }
    HubToBroker::Read(call)
}

fn step_chunk(seq: u64, step: u32, text: &str) -> RunnerToBroker {
    RunnerToBroker::Chunk(Chunk {
        job: JobId("j1".into()),
        seq,
        step: Some(step),
        text: text.into(),
    })
}

#[tokio::test]
async fn log_follow_of_one_step_gets_only_that_step() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    let HubToBroker::Order(mut o) = order("j1", req(512, 1, 60)) else {
        unreachable!()
    };
    let mut test = o.spec.steps[0].clone();
    test.name = "test".into();
    o.spec.steps.push(test);
    hub.send(&HubToBroker::Order(o)).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    r.acked(1).await;
    let mut viewer = h.local_hub("alex").await;
    viewer.send(&follow_step("j1", "test")).await;
    let live = log_chunk(&mut viewer).await;
    assert_eq!((live.text.as_str(), live.end), ("", false));
    r.send(&step_chunk(2, 0, "build\n")).await;
    r.send(&step_chunk(3, 1, "test\n")).await;
    r.acked(3).await;
    assert_eq!(log_chunk(&mut viewer).await.text, "test\n");
}

#[tokio::test]
async fn an_empty_log_of_a_running_job_starts_with_an_empty_frame() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    r.acked(1).await;
    let mut viewer = h.local_hub("alex").await;
    viewer.send(&follow("j1")).await;
    let live = log_chunk(&mut viewer).await;
    assert_eq!((live.req, live.text.as_str(), live.end), (77, "", false));
}

#[tokio::test]
async fn a_closed_viewer_leaves_no_follow_behind() {
    let h = Harness::start("");
    let (mut r, _hub) = running(&h).await;
    let mut viewer = h.local_hub("alex").await;
    viewer.send(&follow("j1")).await;
    log_chunk(&mut viewer).await;
    viewer.close().await;
    h.poke().await;
    r.send(&chunk("j1", 3, "b\n")).await;
    r.acked(3).await;
    r.send(&result("j1", 4, JobState::Passed, 0)).await;
    r.acked(4).await;
    let mut next = h.local_hub("alex").await;
    next.send(&follow("j1")).await;
    let only = log_chunk(&mut next).await;
    assert_eq!((only.text.as_str(), only.end), ("a\nb\n", true));
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
    let ReadReply::Failed { message } = reply else {
        panic!("expected a refusal, got {reply:?}");
    };
    assert!(
        message.contains("subscriptions and followed logs"),
        "{message}"
    );
}
