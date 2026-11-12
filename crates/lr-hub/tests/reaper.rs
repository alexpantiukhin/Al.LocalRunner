mod common;

use common::broker::{FakeBroker, Script};
use common::*;
use lr_abstractions::ProcessRef;
use lr_core::{JobId, JobState};
use lr_proto::{CliReply, CliRequest, ReasonCode};

async fn two_jobs_one_running(rig: &mut Rig) -> (FakeBroker, JobId, JobId) {
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    let queued = cli.submit(submit("unit", None)).await;
    let running = cli.submit(submit("unit", None)).await;
    eventually("both accepted", || local.orders().len() == 2).await;
    settle().await;
    local.started(&running).await;
    settle().await;
    (local, queued, running)
}

#[tokio::test]
async fn dead_sender_cancels_queued_and_kills_running() {
    let mut rig = Rig::new();
    let (local, queued, running) = two_jobs_one_running(&mut rig).await;
    rig.inspector.remove(SENDER_PID);
    rig.step(6).await;
    assert!(
        local.cancels().is_empty(),
        "dead at the 5 s check, not yet confirmed"
    );
    rig.step(1).await;
    eventually("both cancelled", || local.cancels().len() == 2).await;
    let mut jobs: Vec<JobId> = local.cancels().iter().map(|c| c.job.clone()).collect();
    jobs.sort();
    let mut want = vec![queued, running];
    want.sort();
    assert_eq!(jobs, want);
    assert!(local
        .cancels()
        .iter()
        .all(|c| c.code == ReasonCode::SenderGone && !c.force));
}

#[tokio::test]
async fn detach_survives_dead_sender() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let mut req = submit("unit", None);
    req.detach = true;
    let job = rig.cli().await.submit(req).await;
    eventually("accepted", || local.orders().len() == 1).await;
    assert!(rig.active(&job).await.unwrap().detached);
    rig.inspector.remove(SENDER_PID);
    rig.step(20).await;
    assert!(local.cancels().is_empty());
    assert!(rig.active(&job).await.is_some());
}

#[tokio::test]
async fn wait_interrupt_cancels_nothing() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let job = rig.cli().await.submit(submit("unit", None)).await;
    eventually("accepted", || local.orders().len() == 1).await;
    settle().await;
    let watcher = rig.cli().await;
    watcher.send(CliRequest::Wait { job: job.clone() }).await;
    local.started(&job).await;
    local.chunk(&job, "progress\n").await;
    match watcher.reply().await {
        CliReply::Output { job: j, text } => {
            assert_eq!((j, text.as_str()), (job.clone(), "progress\n"))
        }
        other => panic!("{other:?}"),
    }
    watcher.close().await;
    rig.step(10).await;
    assert!(local.cancels().is_empty());
    assert!(rig.active(&job).await.is_some());
    let again = rig.cli().await;
    again.send(CliRequest::Wait { job: job.clone() }).await;
    settle().await;
    local.result(&job, JobState::Failed, Some(3)).await;
    match again.reply().await {
        CliReply::Exit {
            state, exit_code, ..
        } => assert_eq!((state, exit_code), (JobState::Failed, Some(3))),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn sender_alive_again_at_second_check_is_not_reaped() {
    let mut rig = Rig::new();
    let (local, _, _) = two_jobs_one_running(&mut rig).await;
    rig.inspector.remove(SENDER_PID);
    rig.step(6).await;
    rig.inspector.add(ProcessRef {
        pid: SENDER_PID,
        start: Some(SENDER_START),
    });
    rig.step(10).await;
    assert!(local.cancels().is_empty());
}

#[tokio::test]
async fn reused_pid_with_other_start_counts_as_dead() {
    let mut rig = Rig::new();
    let (local, _, _) = two_jobs_one_running(&mut rig).await;
    rig.inspector.add(ProcessRef {
        pid: SENDER_PID,
        start: Some(SENDER_START + 1),
    });
    rig.step(7).await;
    eventually("cancelled", || local.cancels().len() == 2).await;
}

#[tokio::test]
async fn unknown_start_time_never_reaps_and_warns() {
    let mut rig = Rig::new();
    let (local, _, _) = two_jobs_one_running(&mut rig).await;
    rig.inspector.add(ProcessRef {
        pid: SENDER_PID,
        start: None,
    });
    rig.step(30).await;
    assert!(local.cancels().is_empty());
    let text = rig.log_text();
    assert!(
        text.contains("Warn") && text.contains("start time"),
        "{text}"
    );
}

#[tokio::test]
async fn dead_sender_drops_order_without_accepted() {
    let mut rig = Rig::new();
    rig.local = true;
    rig.start();
    let job = rig.cli().await.submit(submit("unit", None)).await;
    rig.inspector.remove(SENDER_PID);
    rig.step(7).await;
    assert!(rig.active(&job).await.is_none());
    assert!(rig.pending().await.is_empty());
}

#[tokio::test]
async fn reaper_checks_at_hub_start() {
    let mut rig = Rig::new();
    rig.local = true;
    let job = JobId("restored".into());
    let order = lr_abstractions::ActiveOrder::new(job.clone(), "agent", "alex", 777, Some(1));
    use lr_abstractions::HubStore;
    rig.store
        .enqueue(
            order,
            lr_abstractions::OutboxKind::Order,
            "{}".into(),
            lr_core::Timestamp(START),
        )
        .await
        .unwrap();
    rig.start();
    settle().await;
    rig.step(2).await;
    assert!(
        rig.active(&job).await.is_none(),
        "dead at start, confirmed 2 s later"
    );
}
