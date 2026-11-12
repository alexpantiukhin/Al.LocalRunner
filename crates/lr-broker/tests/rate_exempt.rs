mod common;

use common::harness::*;
use lr_core::JobId;
use lr_proto::*;

fn rate_limited(reply: &ReadReply) -> bool {
    matches!(reply, ReadReply::Denied { reason } if reason.starts_with("rejected_rate_limited"))
}

async fn log_chunk(c: &mut Conn) -> LogChunk {
    loop {
        if let BrokerToHub::LogChunk(l) = c.hub().await {
            return l;
        }
    }
}

async fn cancel_ack(c: &mut Conn) -> CancelAck {
    loop {
        if let BrokerToHub::CancelAck(a) = c.hub().await {
            return a;
        }
    }
}

#[tokio::test]
async fn cancel_ack_heartbeat_and_log_unfollow_are_never_rate_limited() {
    let h = Harness::start("[limits]\napi_requests_per_min = 4\n");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    r.send(&chunk("j1", 2, "a\n")).await;
    r.acked(2).await;
    let mut viewer = h.local_hub("alex").await;
    viewer
        .send(&HubToBroker::Read(ReadCall {
            req: 77,
            request: ReadRequest::Log {
                job: JobId("j1".into()),
                step: None,
                tail: None,
                follow: true,
                client: None,
            },
        }))
        .await;
    assert_eq!(log_chunk(&mut viewer).await.text, "a\n");
    for seq in [1, 2, 1, 2, 1, 2] {
        hub.send(&HubToBroker::Heartbeat).await;
        hub.send(&HubToBroker::Ack(Ack {
            job: JobId("j1".into()),
            seq,
        }))
        .await;
    }
    assert_eq!(
        viewer.read(ReadRequest::Unfollow { req: 77 }).await,
        ReadReply::Unsubscribed { sub: 77 }
    );
    hub.send(&cancel("j1", 2)).await;
    assert_eq!(
        cancel_ack(&mut viewer).await.outcome,
        CancelOutcome::Applied,
        "the outcome goes to the latest connection of the key"
    );
    for n in 3..=4 {
        let reply = hub.read(ReadRequest::Runners).await;
        assert!(
            matches!(reply, ReadReply::Runners { .. }),
            "request {n} of 4 is within the rate: {reply:?}"
        );
    }
    assert!(
        rate_limited(&hub.read(ReadRequest::Runners).await),
        "the fifth counted request is over the rate"
    );
}
