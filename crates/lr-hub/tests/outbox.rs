mod common;

use common::broker::{OnOrder, Script};
use common::*;
use lr_abstractions::{Frame, OutboxKind};
use lr_core::{JobId, JobState};
use lr_hub::OnExpire;
use lr_proto::{
    BrokerToHub, CancelRequest, CancelTarget, Chunk, CliReply, CliRequest, HubToBroker, JobResult,
    ReasonCode, RejectReason, ResumePoint,
};
use std::time::Duration;

const DAY_MINUS_1S: Duration = Duration::from_secs(24 * 3600 - 1);

fn cancel(job: &JobId, label: &str, force: bool) -> CliRequest {
    CliRequest::Cancel(CancelRequest {
        actor: identity(label),
        target: CancelTarget::Job(job.clone()),
        force,
        reason: Some("not needed".into()),
    })
}

async fn accepted_job(rig: &mut Rig) -> (common::broker::FakeBroker, JobId) {
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let job = rig.cli().await.submit(submit("unit", None)).await;
    eventually("accepted", || local.orders().len() == 1).await;
    settle().await;
    assert!(rig.pending().await.is_empty());
    (local, job)
}

#[tokio::test]
async fn outbox_entry_dropped_after_ttl_when_sender_alive() {
    let mut rig = Rig::new();
    rig.local = true;
    rig.start();
    let cli = rig.cli().await;
    let mut req = submit("unit", None);
    req.wait = true;
    let job = cli.submit(req).await;
    rig.clock.advance(DAY_MINUS_1S);
    settle().await;
    assert_eq!(rig.pending().await.len(), 1, "waits up to ttl_hours = 24");
    rig.step(1).await;
    assert!(rig.pending().await.is_empty());
    assert!(rig.active(&job).await.is_none());
    match cli.reply().await {
        CliReply::Rejected { code, .. } => {
            assert_eq!(code, ReasonCode::Rejected(RejectReason::OutboxExpired))
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn on_expire_keep_waits_past_ttl() {
    let mut rig = Rig::new();
    rig.local = true;
    rig.policy.outbox.on_expire = OnExpire::Keep;
    rig.policy.outbox.ttl_hours = 1;
    rig.start();
    let job = rig.cli().await.submit(submit("unit", None)).await;
    rig.clock.advance(Duration::from_secs(3 * 3600));
    settle().await;
    rig.step(1).await;
    assert_eq!(rig.pending().await, vec![(job, 1, OutboxKind::Order)]);
}

#[tokio::test]
async fn ttl_hours_from_policy_is_used() {
    let mut rig = Rig::new();
    rig.local = true;
    rig.policy.outbox.ttl_hours = 2;
    rig.start();
    let job = rig.cli().await.submit(submit("unit", None)).await;
    rig.clock.advance(Duration::from_secs(2 * 3600 - 1));
    settle().await;
    assert!(rig.active(&job).await.is_some());
    rig.step(1).await;
    assert!(rig.active(&job).await.is_none());
}

#[tokio::test]
async fn cancel_before_any_broker_drops_the_order_locally() {
    let mut rig = Rig::new();
    rig.local = true;
    rig.start();
    let cli = rig.cli().await;
    let job = cli.submit(submit("unit", None)).await;
    cli.send(cancel(&job, "agent", false)).await;
    assert_eq!(
        cli.reply().await,
        CliReply::Cancelled {
            jobs: vec![job.clone()]
        }
    );
    assert!(rig.pending().await.is_empty());
    assert!(rig.active(&job).await.is_none());
}

#[tokio::test]
async fn cancel_before_accepted_still_reaches_the_broker() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner().on_order(OnOrder::Silent));
    rig.start();
    let cli = rig.cli().await;
    let job = cli.submit(submit("unit", None)).await;
    eventually("order sent", || local.orders().len() == 1).await;
    cli.send(cancel(&job, "agent", false)).await;
    assert!(matches!(cli.reply().await, CliReply::Cancelled { .. }));
    eventually("cancel sent", || local.cancels().len() == 1).await;
    let c = &local.cancels()[0];
    assert_eq!((c.seq, c.code.clone()), (2, ReasonCode::CancelledByUser));
    assert_eq!(c.reason.as_deref(), Some("not needed"));
    settle().await;
    assert!(
        rig.pending()
            .await
            .iter()
            .all(|(_, _, kind)| *kind != OutboxKind::Order),
        "the order entry is gone"
    );
}

#[tokio::test]
async fn cancel_after_accepted_waits_in_outbox_until_acked() {
    let mut rig = Rig::new();
    let (local, job) = accepted_job(&mut rig).await;
    let cli = rig.cli().await;
    cli.send(cancel(&job, "agent", false)).await;
    assert!(matches!(cli.reply().await, CliReply::Cancelled { .. }));
    eventually("cancel sent", || local.cancels().len() == 1).await;
    settle().await;
    assert!(rig.pending().await.is_empty(), "acked by the broker");
    assert!(
        rig.active(&job).await.is_some(),
        "final state comes from the broker"
    );
}

#[tokio::test]
async fn foreign_label_cancel_needs_force_and_is_logged() {
    let mut rig = Rig::new();
    let (local, job) = accepted_job(&mut rig).await;
    let cli = rig.cli().await;
    cli.send(cancel(&job, "intruder", false)).await;
    assert!(matches!(cli.reply().await, CliReply::Error(_)));
    settle().await;
    assert!(local.cancels().is_empty());
    cli.send(cancel(&job, "intruder", true)).await;
    assert!(matches!(cli.reply().await, CliReply::Cancelled { .. }));
    eventually("cancel sent", || local.cancels().len() == 1).await;
    assert!(local.cancels()[0].force);
    let log = String::from_utf8(rig.store.file(&log_path(&job)).unwrap()).unwrap();
    assert!(log.contains("intruder") && log.contains("--force"), "{log}");
}

#[tokio::test]
async fn cancel_mine_touches_only_own_active_jobs() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    let mine = cli.submit(submit("unit", None)).await;
    let mut other = submit("unit", None);
    other.identity.label = "someone".into();
    let theirs = cli.submit(other).await;
    eventually("accepted", || local.orders().len() == 2).await;
    settle().await;
    cli.send(CliRequest::Cancel(CancelRequest {
        actor: identity("agent"),
        target: CancelTarget::Mine,
        force: false,
        reason: None,
    }))
    .await;
    assert_eq!(
        cli.reply().await,
        CliReply::Cancelled {
            jobs: vec![mine.clone()]
        }
    );
    eventually("cancel sent", || local.cancels().len() == 1).await;
    assert_eq!(local.cancels()[0].job, mine);
    assert!(rig.active(&theirs).await.is_some());
}

#[tokio::test]
async fn broker_messages_deduplicated_by_seq_and_acked() {
    let mut rig = Rig::new();
    let (local, job) = accepted_job(&mut rig).await;
    let chunk = |seq: u64, text: &str| {
        BrokerToHub::Chunk(Chunk {
            job: job.clone(),
            seq,
            step: Some(0),
            text: text.into(),
        })
    };
    local.send(chunk(1, "first\n")).await;
    local.send(chunk(1, "first\n")).await;
    settle().await;
    let log = String::from_utf8(rig.store.file(&log_path(&job)).unwrap()).unwrap();
    assert_eq!(log.matches("first\n").count(), 1, "{log}");
    assert_eq!(
        local.acks(),
        vec![(job.clone(), 1), (job.clone(), 1)],
        "a repeat is acked again"
    );
    assert_eq!(rig.active(&job).await.unwrap().last_seq, 1);
    local.send(chunk(3, "gap\n")).await;
    settle().await;
    rig.step(2).await;
    eventually("reconnected after a seq gap", || local.hellos() == 2).await;
    let log = String::from_utf8(rig.store.file(&log_path(&job)).unwrap()).unwrap();
    assert!(!log.contains("gap"), "{log}");
    assert_eq!(rig.active(&job).await.unwrap().last_seq, 1);
}

#[tokio::test]
async fn resume_reports_last_seq_per_job_after_reconnect() {
    let mut rig = Rig::new();
    let (local, job) = accepted_job(&mut rig).await;
    local.chunk(&job, "x\n").await;
    settle().await;
    local.disconnect().await;
    rig.step(2).await;
    eventually("reconnected", || local.hellos() == 2).await;
    settle().await;
    let resumes: Vec<Vec<ResumePoint>> = local
        .received()
        .into_iter()
        .filter_map(|m| match m {
            HubToBroker::Resume(r) => Some(r.points),
            _ => None,
        })
        .collect();
    assert_eq!(
        resumes.last().unwrap(),
        &vec![ResumePoint {
            job: job.clone(),
            last_seq: 1
        }]
    );
}

#[tokio::test]
async fn hub_absent_cancel_after_resume_is_shown_and_not_resent() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    let mut req = submit("unit", None);
    req.wait = true;
    let job = cli.submit(req).await;
    eventually("accepted", || local.orders().len() == 1).await;
    settle().await;
    local.disconnect().await;
    rig.step(2).await;
    eventually("reconnected", || local.hellos() == 2).await;
    settle().await;

    let seq = local.next_seq(&job);
    local
        .send(BrokerToHub::JobResult(JobResult {
            job: job.clone(),
            seq,
            state: JobState::Cancelled,
            exit_code: None,
            reason: Some(ReasonCode::HubAbsent),
            detail: None,
            peak_disk_mb: None,
            peak_mem_mb: None,
        }))
        .await;

    assert_eq!(
        cli.reply().await,
        CliReply::Exit {
            job: job.clone(),
            state: JobState::Cancelled,
            exit_code: None,
            reason: Some(ReasonCode::HubAbsent),
        }
    );
    settle().await;
    assert!(rig.active(&job).await.is_none());
    let log = String::from_utf8(rig.store.file(&log_path(&job)).unwrap()).unwrap();
    assert!(log.contains("cancelled, hub_absent"), "{log}");
    local.disconnect().await;
    rig.step(10).await;
    eventually("reconnected again", || local.hellos() == 3).await;
    settle().await;
    assert_eq!(
        local.orders().len(),
        1,
        "the cancelled order is not sent again"
    );
    assert!(rig.pending().await.is_empty());
}

#[tokio::test]
async fn unacknowledged_order_resent_after_reconnect() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner().on_order(OnOrder::Silent));
    rig.start();
    let job = rig.cli().await.submit(submit("unit", None)).await;
    eventually("order sent", || local.orders().len() == 1).await;
    local.disconnect().await;
    rig.step(2).await;
    eventually("order sent again", || local.orders().len() == 2).await;
    assert!(local.orders().iter().all(|o| o.job == job && o.seq == 1));
}

#[tokio::test]
async fn denied_cancel_ack_without_seq_is_understood() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner().on_order(OnOrder::Silent));
    rig.start();
    let cli = rig.cli().await;
    let job = cli.submit(submit("unit", None)).await;
    eventually("order sent", || local.orders().len() == 1).await;
    cli.send(cancel(&job, "agent", false)).await;
    let _ = cli.reply().await;
    eventually("cancel sent", || local.cancels().len() == 1).await;
    let denied = serde_json::json!({
        "type": "cancel_ack", "job": job.0, "seq": 0, "cancel_seq": 2,
        "outcome": "denied", "denial": "заказ не найден"
    });
    local
        .send_frame(Frame::from_bytes(serde_json::to_vec(&denied).unwrap()))
        .await;
    settle().await;
    assert!(
        rig.active(&job).await.is_none(),
        "never accepted: cancelled locally"
    );
}

#[tokio::test]
async fn jobs_mine_lists_active_jobs_of_the_label() {
    let mut rig = Rig::new();
    let (local, job) = accepted_job(&mut rig).await;
    local.started(&job).await;
    settle().await;
    let cli = rig.cli().await;
    cli.send(CliRequest::Jobs {
        label: "agent".into(),
        active: true,
    })
    .await;
    match cli.reply().await {
        CliReply::Jobs { jobs } => {
            assert_eq!(jobs.len(), 1);
            assert_eq!(jobs[0].job, job);
            assert_eq!(jobs[0].state, lr_core::JobState::Running);
            assert_eq!(jobs[0].label.as_deref(), Some("agent"));
        }
        other => panic!("{other:?}"),
    }
    cli.send(CliRequest::Jobs {
        label: "nobody".into(),
        active: true,
    })
    .await;
    assert_eq!(cli.reply().await, CliReply::Jobs { jobs: vec![] });
}

#[tokio::test]
async fn brokers_lists_zone_availability_and_outbox() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner().on_order(OnOrder::Silent));
    rig.start();
    let cli = rig.cli().await;
    cli.submit(submit("unit", None)).await;
    eventually("order sent", || local.orders().len() == 1).await;
    cli.send(CliRequest::Brokers).await;
    match cli.reply().await {
        CliReply::Brokers { brokers } => {
            assert_eq!(brokers.len(), 1);
            assert_eq!(brokers[0].name, "local");
            assert!(brokers[0].available);
            assert_eq!(brokers[0].zone, Some(lr_core::Zone::Local));
            assert_eq!(brokers[0].outbox, 1);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn repeated_cancel_of_one_job_queues_one_cancel() {
    let mut rig = Rig::new();
    let (local, job) = accepted_job(&mut rig).await;
    let cli = rig.cli().await;
    for _ in 0..2 {
        cli.send(cancel(&job, "agent", false)).await;
        assert_eq!(
            cli.reply().await,
            CliReply::Cancelled {
                jobs: vec![job.clone()]
            }
        );
    }
    settle().await;
    assert_eq!(local.cancels().len(), 1);
    let log = String::from_utf8(rig.store.file(&log_path(&job)).unwrap()).unwrap();
    assert_eq!(log.matches("cancel requested").count(), 1, "{log}");
}
