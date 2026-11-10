mod common;

use common::harness::*;
use lr_abstractions::BrokerStore;
use lr_core::{JobId, JobState};
use lr_proto::*;

const ALEX: &str = "local:alex";

async fn busy_pool(h: &Harness) -> (Conn, Conn) {
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    hub.send(&order("j2", req(512, 1, 60))).await;
    hub.accepted().await;
    (r, hub)
}

async fn cancel_ack(c: &mut Conn) -> CancelAck {
    loop {
        if let BrokerToHub::CancelAck(a) = c.hub().await {
            return a;
        }
    }
}

fn cancel_for(job: &str, seq: u64, client: &str) -> HubToBroker {
    let HubToBroker::Cancel(mut c) = cancel(job, seq) else {
        unreachable!()
    };
    c.client = Some(client.into());
    HubToBroker::Cancel(c)
}

#[tokio::test]
async fn repeated_order_with_a_later_seq_does_not_take_the_cancel_seq() {
    let h = Harness::start("");
    let (_r, mut hub) = busy_pool(&h).await;
    let HubToBroker::Order(mut o) = order("j2", req(512, 1, 60)) else {
        unreachable!()
    };
    o.seq = 2;
    hub.send(&HubToBroker::Order(o)).await;
    match hub.hub().await {
        BrokerToHub::Error(e) => assert_eq!(e.code, ProtocolErrorCode::Malformed),
        other => panic!("expected refusal, got {other:?}"),
    }
    hub.send(&cancel("j2", 2)).await;
    assert_eq!(cancel_ack(&mut hub).await.outcome, CancelOutcome::Applied);
    assert_eq!(
        h.job_row(ALEX, "j2").await.unwrap().job.state,
        JobState::Cancelled
    );
}

#[tokio::test]
async fn cancel_for_an_offline_runner_is_sent_when_it_returns() {
    let h = Harness::start("");
    let (r, mut hub) = busy_pool(&h).await;
    r.send(&started("j1", 1)).await;
    hub.started_notice().await;
    r.close().await;
    hub.until_runner("r1", false).await;
    hub.send(&cancel("j1", 2)).await;
    assert_eq!(cancel_ack(&mut hub).await.outcome, CancelOutcome::Applied);
    let mut back = h.dial(member_peer("pk-r1", "10.0.0.7")).await;
    back.send(&hello("r1", Some(linux()))).await;
    back.welcome().await;
    let c = loop {
        if let BrokerToRunner::Cancel(c) = back.runner().await {
            break c;
        }
    };
    assert_eq!((c.job, c.seq), (JobId("j1".into()), 2));
}

#[tokio::test]
async fn job_that_passes_despite_a_cancel_keeps_no_canceller() {
    let h = Harness::start("");
    let (mut r, mut hub) = busy_pool(&h).await;
    hub.send(&cancel("j1", 2)).await;
    cancel_ack(&mut hub).await;
    loop {
        if let BrokerToRunner::Cancel(_) = r.runner().await {
            break;
        }
    }
    r.send(&result("j1", 1, JobState::Passed, 0)).await;
    let res = loop {
        if let BrokerToHub::JobResult(x) = hub.hub().await {
            break x;
        }
    };
    assert_eq!((res.state, res.reason), (JobState::Passed, None));
    let row = h.job_row(ALEX, "j1").await.unwrap().job;
    assert_eq!(row.state, JobState::Passed);
    assert_eq!((row.cancelled_by, row.cancel_reason), (None, None));
}

#[tokio::test]
async fn owner_names_the_client_when_two_clients_use_one_job_id() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&order("j0", req(512, 1, 60))).await;
    alex.accepted().await;
    r.assign().await;
    alex.send(&order("j1", req(512, 1, 60))).await;
    alex.accepted().await;
    let mut bob = h.local_hub("bob").await;
    bob.send(&order("j1", req(512, 1, 60))).await;
    bob.accepted().await;
    let mut root = h.local_hub("root").await;
    root.send(&cancel("j1", 1)).await;
    let ack = cancel_ack(&mut root).await;
    assert_eq!(ack.outcome, CancelOutcome::Denied);
    assert!(ack.denial.unwrap().contains("local:bob"));
    root.send(&cancel_for("j1", 2, "local:bob")).await;
    assert_eq!(cancel_ack(&mut root).await.outcome, CancelOutcome::Applied);
    assert_eq!(
        h.job_row("local:bob", "j1").await.unwrap().job.state,
        JobState::Cancelled
    );
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Queued
    );
}

#[tokio::test]
async fn denied_cancels_leave_no_outbox_rows() {
    let h = Harness::start("");
    let (_r, _alex) = busy_pool(&h).await;
    let mut bob = h.local_hub("bob").await;
    for (i, job) in ["nope", "j2", "other"].iter().enumerate() {
        bob.send(&cancel(job, i as u64 + 1)).await;
        assert_eq!(cancel_ack(&mut bob).await.outcome, CancelOutcome::Denied);
    }
    assert!(h
        .store
        .pending_results("local:bob")
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn cancel_is_sent_again_when_the_runner_reconnects_over_a_live_connection() {
    let h = Harness::start("");
    let (mut r, mut hub) = busy_pool(&h).await;
    r.send(&started("j1", 1)).await;
    hub.started_notice().await;
    hub.send(&cancel("j1", 2)).await;
    loop {
        if let BrokerToRunner::Cancel(_) = r.runner().await {
            break;
        }
    }
    let mut back = h.dial(member_peer("pk-r1", "10.0.0.7")).await;
    back.send(&hello("r1", Some(linux()))).await;
    back.welcome().await;
    let c = loop {
        if let BrokerToRunner::Cancel(c) = back.runner().await {
            break c;
        }
    };
    assert_eq!(c.job, JobId("j1".into()));
}

#[tokio::test]
async fn cancel_by_wrong_key_refused() {
    let h = Harness::start("[cancel]\nothers = \"owner\"\n");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut owner = h.member_hub("laptop", "agent", "10.0.0.20").await;
    owner.send(&order("j1", req(512, 1, 60))).await;
    owner.accepted().await;
    owner.send(&order("j2", req(512, 1, 60))).await;
    owner.accepted().await;
    let mut other = h.member_hub("desk", "agent", "10.0.0.21").await;
    other.send(&cancel_for("j2", 1, "laptop")).await;
    let ack = cancel_ack(&mut other).await;
    assert_eq!(ack.outcome, CancelOutcome::Denied);
    assert!(ack.denial.unwrap().contains("[cancel] others"));
    assert_eq!(
        h.job_row("laptop", "j2").await.unwrap().job.state,
        JobState::Queued
    );
}

#[tokio::test]
async fn sender_gone_cancel_keeps_its_reason_code() {
    let h = Harness::start("");
    let (_r, mut hub) = busy_pool(&h).await;
    let HubToBroker::Cancel(mut c) = cancel("j2", 2) else {
        unreachable!()
    };
    c.code = ReasonCode::SenderGone;
    c.reason = None;
    hub.send(&HubToBroker::Cancel(c)).await;
    assert_eq!(cancel_ack(&mut hub).await.outcome, CancelOutcome::Applied);
    let row = h.job_row(ALEX, "j2").await.unwrap();
    assert_eq!(row.job.state, JobState::Cancelled);
    assert_eq!(row.job.cancel_reason.as_deref(), Some("sender_gone"));
}
