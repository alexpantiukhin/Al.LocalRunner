mod common;

use common::harness::*;
use lr_abstractions::{BrokerStore, JobKey};
use lr_core::{JobId, JobState};
use lr_proto::*;

const ALEX: &str = "local:alex";

#[tokio::test]
async fn order_accepted_then_assigned_to_matching_runner() {
    let h = Harness::start("");
    let mut mac = h
        .member_runner("r-mac", "10.0.0.8", caps(&["mac"], 16384, 8))
        .await;
    let mut lin = h.member_runner("r-linux", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(1024, 2, 600))).await;
    let a = hub.accepted().await;
    assert_eq!(a.job, JobId("j1".into()));
    assert_eq!(a.order_seq, 1);
    assert_eq!(a.waiting, None);
    let row = h.job_row(ALEX, "j1").await.expect("stored before Accepted");
    assert_eq!(row.job.client_label, "");
    assert_eq!(row.job.project, "owner/repo");
    let assign = lin.assign().await;
    assert_eq!(assign.job, JobId("j1".into()));
    assert_eq!(assign.seq, 1);
    assert_eq!(assign.spec.client, ALEX);
    assert_eq!(assign.spec.steps[0].command, "make");
    assert_eq!(
        assign.spec.code,
        WireCode::Commit {
            sha: "abc".into(),
            repo_url: None,
            branch: None
        }
    );
    assert_eq!(assign.spec.broker_lost_kill_s, Some(180));
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.runner.as_deref(),
        Some("r-linux")
    );
    assert!(h.store.orders().await.unwrap().is_empty());
    mac.quiet().await;
}

#[tokio::test]
async fn exceeding_pool_rejected_at_intake() {
    let h = Harness::start("");
    let _r = h
        .member_runner("r1", "10.0.0.7", caps(&["linux"], 65536, 32))
        .await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(20000, 1, 60))).await;
    let r = hub.rejected().await;
    assert_eq!(r.code, ReasonCode::Rejected(RejectReason::PoolMemory));
    assert!(
        r.detail.contains("20000") && r.detail.contains("16384"),
        "{}",
        r.detail
    );
    assert!(h.job_row(ALEX, "j1").await.is_none());
}

#[tokio::test]
async fn intake_rejects_cpu_above_pool() {
    let h = Harness::start("");
    let _r = h
        .member_runner("r1", "10.0.0.7", caps(&["linux"], 65536, 32))
        .await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 12, 60))).await;
    let r = hub.rejected().await;
    assert_eq!(r.code, ReasonCode::Rejected(RejectReason::PoolCpu));
    assert!(
        r.detail.contains("12") && r.detail.contains('8'),
        "{}",
        r.detail
    );
}

#[tokio::test]
async fn intake_rejects_timeout_above_broker_limit() {
    let h = Harness::start("[limits]\nmax_job_timeout_s = 600\n");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 1200))).await;
    let r = hub.rejected().await;
    assert_eq!(r.code, ReasonCode::Rejected(RejectReason::Timeout));
    assert!(
        r.detail.contains("600") && r.detail.contains("1200"),
        "{}",
        r.detail
    );
}

#[tokio::test]
async fn intake_rejects_when_no_registered_runner_suits_with_figures() {
    let h = Harness::start("");
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j0", req(512, 1, 60))).await;
    let none = hub.rejected().await;
    assert_eq!(
        none.code,
        ReasonCode::Rejected(RejectReason::NoMatchingRunner)
    );
    assert!(none.detail.contains("нет зарегистрированных раннеров"));
    let _r = h
        .member_runner("r-small", "10.0.0.7", caps(&["linux"], 4096, 8))
        .await;
    hub.send(&order("j1", req(8192, 1, 60))).await;
    let r = hub.rejected().await;
    assert_eq!(r.code, ReasonCode::Rejected(RejectReason::NoMatchingRunner));
    assert!(
        r.detail.contains("r-small") && r.detail.contains("8192") && r.detail.contains("4096"),
        "{}",
        r.detail
    );
}

#[tokio::test]
async fn intake_rejects_order_squeezed_out_by_reserve_rule() {
    let h = Harness::start(
        "[[rule]]\nname = \"ci-reserve\"\nwhen = { client_label = \"ci\" }\n\
         reserve = { cpu = 6, mem_mb = 1024 }\n",
    );
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 4, 60))).await;
    let r = hub.rejected().await;
    assert_eq!(r.code, ReasonCode::Rejected(RejectReason::Reserved));
    assert!(r.detail.contains("ci-reserve"), "{}", r.detail);
}

#[tokio::test]
async fn intake_rejects_order_of_group_with_zero_quota() {
    let h = Harness::start("[[rule]]\nname = \"frozen\"\nquota = { max_running = 0 }\n");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    let r = hub.rejected().await;
    assert_eq!(r.code, ReasonCode::Rejected(RejectReason::Quota));
    assert!(r.detail.contains("frozen"), "{}", r.detail);
}

#[tokio::test]
async fn intake_rejects_over_max_active_per_client() {
    let h = Harness::start("[limits]\nmax_active_per_client = 1\n");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&order("j1", req(512, 1, 60))).await;
    alex.accepted().await;
    alex.send(&order("j2", req(512, 1, 60))).await;
    let r = alex.rejected().await;
    assert_eq!(r.code, ReasonCode::Rejected(RejectReason::QueueLimit));
    assert!(r.detail.contains('1'), "{}", r.detail);
    let mut bob = h.local_hub("bob").await;
    bob.send(&order("j3", req(512, 1, 60))).await;
    bob.accepted().await;
}

#[tokio::test]
async fn default_max_active_per_client_is_100() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    for i in 0..100 {
        hub.send(&order(&format!("j{i}"), req(512, 1, 60))).await;
        hub.accepted().await;
    }
    r.assign().await;
    hub.send(&order("j100", req(512, 1, 60))).await;
    assert_eq!(
        hub.rejected().await.code,
        ReasonCode::Rejected(RejectReason::QueueLimit)
    );
}

#[tokio::test]
async fn order_waits_when_every_suitable_runner_is_offline() {
    let h = Harness::start("");
    let r = h.member_runner("r1", "10.0.0.7", linux()).await;
    r.close().await;
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("r1", false).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    let a = hub.accepted().await;
    assert_eq!(a.waiting.as_deref(), Some("подходящие раннеры offline"));
}

#[tokio::test]
async fn accepted_names_why_the_order_waits() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    hub.send(&order("j2", req(512, 1, 60))).await;
    assert_eq!(
        hub.accepted().await.waiting.as_deref(),
        Some("раннер занят")
    );
}

#[tokio::test]
async fn new_order_must_open_its_stream_at_seq_1() {
    let h = Harness::start("");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    let HubToBroker::Order(mut o) = order("j1", req(512, 1, 60)) else {
        unreachable!()
    };
    o.seq = 3;
    hub.send(&HubToBroker::Order(o)).await;
    match hub.hub().await {
        BrokerToHub::Error(e) => assert_eq!(e.code, ProtocolErrorCode::SeqGap),
        other => panic!("expected seq error, got {other:?}"),
    }
    assert!(h.job_row(ALEX, "j1").await.is_none());
}

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

fn cancel_ack(m: BrokerToHub) -> CancelAck {
    match m {
        BrokerToHub::CancelAck(a) => a,
        other => panic!("expected cancel ack, got {other:?}"),
    }
}

#[tokio::test]
async fn cancel_of_queued_removes_immediately() {
    let h = Harness::start("");
    let (mut r, mut hub) = busy_pool(&h).await;
    hub.send(&cancel("j2", 2)).await;
    let mut got = Vec::new();
    for _ in 0..3 {
        got.push(hub.hub().await);
    }
    assert!(got.contains(&BrokerToHub::Ack(Ack {
        job: JobId("j2".into()),
        seq: 2
    })));
    let ack = got
        .iter()
        .find_map(|m| match m {
            BrokerToHub::CancelAck(a) => Some(a.clone()),
            _ => None,
        })
        .expect("cancel ack");
    assert_eq!(ack.outcome, CancelOutcome::Applied);
    assert_eq!(ack.cancel_seq, 2);
    let res = got
        .iter()
        .find_map(|m| match m {
            BrokerToHub::JobResult(r) => Some(r.clone()),
            _ => None,
        })
        .expect("final result");
    assert_eq!(res.state, JobState::Cancelled);
    assert_eq!(res.reason, Some(ReasonCode::CancelledByUser));
    let row = h.job_row(ALEX, "j2").await.unwrap().job;
    assert_eq!(row.state, JobState::Cancelled);
    assert_eq!(row.cancelled_by.as_deref(), Some("agent-1"));
    assert!(row.cancel_reason.unwrap().contains("cancelled_by_user"));
    assert!(h.store.orders().await.unwrap().is_empty());
    r.send(&started("j1", 1)).await;
    r.send(&result("j1", 2, JobState::Passed, 0)).await;
    hub.sync().await;
    r.runner().await;
    r.runner().await;
    r.quiet().await;
}

#[tokio::test]
async fn cancel_other_clients_order_refused() {
    let h = Harness::start("");
    let (_r, mut alex) = busy_pool(&h).await;
    let mut bob = h.local_hub("bob").await;
    bob.send(&cancel("j2", 1)).await;
    let ack = loop {
        if let BrokerToHub::CancelAck(a) = bob.hub().await {
            break a;
        }
    };
    assert_eq!(ack.outcome, CancelOutcome::Denied);
    assert!(ack.denial.is_some());
    assert_eq!(
        h.job_row(ALEX, "j2").await.unwrap().job.state,
        JobState::Queued
    );
    assert_eq!(h.store.orders().await.unwrap().len(), 1);
    assert!(alex.sync().await.is_empty());
}

#[tokio::test]
async fn broker_owner_cancels_others_order_when_policy_allows() {
    let h = Harness::start("[cancel]\nothers = \"owner\"\n");
    let (_r, mut alex) = busy_pool(&h).await;
    let mut root = h.local_hub("root").await;
    root.send(&cancel("j2", 1)).await;
    let ack = loop {
        if let BrokerToHub::CancelAck(a) = root.hub().await {
            break a;
        }
    };
    assert_eq!(ack.outcome, CancelOutcome::Applied);
    assert_eq!(
        h.job_row(ALEX, "j2").await.unwrap().job.state,
        JobState::Cancelled
    );
    let to_alex = alex.sync().await;
    assert!(to_alex
        .iter()
        .any(|m| matches!(m, BrokerToHub::JobResult(r) if r.state == JobState::Cancelled)));
}

#[tokio::test]
async fn default_cancel_others_is_owner() {
    let h = Harness::start("");
    let (_r, _alex) = busy_pool(&h).await;
    let mut root = h.local_hub("root").await;
    root.send(&cancel("j2", 1)).await;
    let ack = loop {
        if let BrokerToHub::CancelAck(a) = root.hub().await {
            break a;
        }
    };
    assert_eq!(ack.outcome, CancelOutcome::Applied);
}

#[tokio::test]
async fn cancel_others_none_refuses_even_the_owner() {
    let h = Harness::start("[cancel]\nothers = \"none\"\n");
    let (_r, _alex) = busy_pool(&h).await;
    let mut root = h.local_hub("root").await;
    root.send(&cancel("j2", 1)).await;
    let ack = loop {
        if let BrokerToHub::CancelAck(a) = root.hub().await {
            break a;
        }
    };
    assert_eq!(ack.outcome, CancelOutcome::Denied);
    assert_eq!(
        h.job_row(ALEX, "j2").await.unwrap().job.state,
        JobState::Queued
    );
}

#[tokio::test]
async fn cancel_of_running_job_waits_for_the_runner_report() {
    let h = Harness::start("");
    let (mut r, mut hub) = busy_pool(&h).await;
    r.send(&started("j1", 1)).await;
    r.acked(1).await;
    hub.send(&cancel("j1", 2)).await;
    let mut got = hub.sync().await;
    got.retain(|m| matches!(m, BrokerToHub::CancelAck(_)));
    assert_eq!(cancel_ack(got.remove(0)).outcome, CancelOutcome::Applied);
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Cancelling
    );
    let c = loop {
        if let BrokerToRunner::Cancel(c) = r.runner().await {
            break c;
        }
    };
    assert_eq!(c.code, ReasonCode::CancelledByUser);
    assert_eq!(c.seq, 2);
    r.send(&result("j1", 2, JobState::Cancelled, 1)).await;
    let res = loop {
        if let BrokerToHub::JobResult(x) = hub.hub().await {
            break x;
        }
    };
    assert_eq!(res.state, JobState::Cancelled);
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Cancelled
    );
}

#[tokio::test]
async fn cancel_of_assigned_but_not_started_goes_to_the_runner() {
    let h = Harness::start("");
    let (mut r, mut hub) = busy_pool(&h).await;
    hub.send(&cancel("j1", 2)).await;
    let c = loop {
        if let BrokerToRunner::Cancel(c) = r.runner().await {
            break c;
        }
    };
    assert_eq!(c.job, JobId("j1".into()));
    assert_eq!(
        h.job_row(ALEX, "j1").await.unwrap().job.state,
        JobState::Queued
    );
    r.send(&result("j1", 1, JobState::Cancelled, 1)).await;
    let res = loop {
        if let BrokerToHub::JobResult(x) = hub.hub().await {
            break x;
        }
    };
    assert_eq!(res.state, JobState::Cancelled);
    assert_eq!(r.assign().await.job, JobId("j2".into()));
}

#[tokio::test]
async fn cancel_of_finished_job_reports_final_state() {
    let h = Harness::start("");
    let (mut r, mut hub) = busy_pool(&h).await;
    r.send(&started("j1", 1)).await;
    r.send(&result("j1", 2, JobState::Passed, 0)).await;
    r.acked(2).await;
    hub.sync().await;
    hub.send(&cancel("j1", 2)).await;
    let ack = loop {
        if let BrokerToHub::CancelAck(a) = hub.hub().await {
            break a;
        }
    };
    assert_eq!(ack.outcome, CancelOutcome::AlreadyFinal);
    let row = h.job_row(ALEX, "j1").await.unwrap().job;
    assert_eq!(row.state, JobState::Passed);
    assert_eq!(row.cancelled_by, None);
}

#[tokio::test]
async fn cancel_of_unknown_job_is_denied() {
    let h = Harness::start("");
    let mut hub = h.local_hub("alex").await;
    hub.send(&cancel("nope", 1)).await;
    let ack = loop {
        if let BrokerToHub::CancelAck(a) = hub.hub().await {
            break a;
        }
    };
    assert_eq!(ack.outcome, CancelOutcome::Denied);
    assert!(h
        .store
        .job(&JobKey::new(ALEX, "nope"))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn cancel_seq_gap_is_a_protocol_error() {
    let h = Harness::start("");
    let (_r, mut hub) = busy_pool(&h).await;
    hub.send(&cancel("j2", 5)).await;
    match hub.hub().await {
        BrokerToHub::Error(e) => assert_eq!(e.code, ProtocolErrorCode::SeqGap),
        other => panic!("expected seq error, got {other:?}"),
    }
    assert_eq!(
        h.job_row(ALEX, "j2").await.unwrap().job.state,
        JobState::Queued
    );
}

#[tokio::test]
async fn label_and_machine_id_come_from_the_key_record() {
    let h = Harness::start("");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut ci = h.member_hub("ci-box", "ci", "10.0.0.20").await;
    ci.send(&order("c1", req(512, 1, 60))).await;
    ci.accepted().await;
    let row = h.job_row("ci-box", "c1").await.unwrap().job;
    assert_eq!(row.client_label, "ci");
    assert_eq!(row.client_machine, "mid-ci-box");
    assert_eq!(row.os_user, "alex");
}
