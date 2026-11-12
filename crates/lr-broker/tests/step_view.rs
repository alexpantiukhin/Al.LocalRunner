mod common;

use common::harness::*;
use lr_core::{JobId, JobState, StepState, Timestamp};
use lr_proto::*;
use std::time::Duration;

fn two_steps(job: &str) -> HubToBroker {
    let HubToBroker::Order(mut o) = order(job, req(512, 1, 600)) else {
        unreachable!()
    };
    let mut test = o.spec.steps[0].clone();
    test.name = "test".into();
    test.timeout_s = 120;
    o.spec.steps.push(test);
    HubToBroker::Order(o)
}

fn step_result(job: &str, seq: u64, ord: u32, state: StepState, ms: u64) -> RunnerToBroker {
    RunnerToBroker::StepResult(StepResult {
        job: JobId(job.into()),
        seq,
        ord,
        name: if ord == 0 { "build" } else { "test" }.into(),
        state,
        exit_code: Some(if state == StepState::Passed { 0 } else { 1 }),
        duration_ms: ms,
    })
}

fn running(reply: ReadReply) -> RunningView {
    match reply {
        ReadReply::Status(mut s) => s.running.remove(0),
        other => panic!("expected status, got {other:?}"),
    }
}

fn details(reply: ReadReply) -> Vec<JobDetail> {
    match reply {
        ReadReply::Details { jobs, .. } => jobs,
        other => panic!("expected details, got {other:?}"),
    }
}

#[tokio::test]
async fn the_running_step_and_step_times_reach_status_and_the_job_timeout_is_in_the_summary() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&two_steps("j1")).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    hub.started_notice().await;
    let begun = h.job_row("local:alex", "j1").await.unwrap().job.started_at;
    let view = running(hub.read(ReadRequest::Status(StatusFilter::default())).await);
    assert_eq!(view.summary.timeout_s, Some(600));
    assert_eq!(view.steps[0].state, StepState::Running);
    assert_eq!(view.steps[0].started_at, begun);
    assert_eq!(view.steps[1].state, StepState::Queued);

    h.clock.advance(Duration::from_secs(5));
    r.send(&step_result("j1", 2, 0, StepState::Passed, 4_000))
        .await;
    r.acked(2).await;
    let view = running(hub.read(ReadRequest::Status(StatusFilter::default())).await);
    let build = &view.steps[0];
    let end = build.finished_at.expect("the step ended");
    assert_eq!(build.state, StepState::Passed);
    assert_eq!(build.started_at, Some(Timestamp(end.0 - 4_000)));
    assert_eq!(view.steps[1].state, StepState::Running);
    assert_eq!(view.steps[1].started_at, Some(end));
}

#[tokio::test]
async fn history_details_carry_steps_and_the_canceller_and_stay_masked_for_others() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&two_steps("j1")).await;
    alex.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    alex.started_notice().await;
    alex.send(&order_at("j2", req(512, 1, 60), START + 1)).await;
    alex.accepted().await;
    alex.send(&cancel("j2", 2)).await;
    h.until_state("local:alex", "j2", JobState::Cancelled).await;
    r.send(&step_result("j1", 2, 0, StepState::Failed, 1_000))
        .await;
    r.acked(2).await;
    let view = running(
        alex.read(ReadRequest::Status(StatusFilter::default()))
            .await,
    );
    assert_eq!(
        view.steps[1].state,
        StepState::Queued,
        "no step after a failed one is shown running"
    );
    r.send(&step_result("j1", 3, 1, StepState::Skipped, 0))
        .await;
    r.send(&result("j1", 4, JobState::Failed, 1)).await;
    r.acked(4).await;
    h.until_state("local:alex", "j1", JobState::Failed).await;

    let ReadReply::History { jobs, .. } = alex
        .read(ReadRequest::History(HistoryFilter::default()))
        .await
    else {
        panic!("history")
    };
    let listed = jobs.iter().find(|s| s.job.0 == "j1").expect("j1 listed");
    assert_eq!(
        listed.timeout_s,
        Some(600),
        "the list carries the stored job timeout"
    );

    let all = details(
        alex.read(ReadRequest::HistoryDetails(HistoryFilter::default()))
            .await,
    );
    let j1 = all.iter().find(|d| d.summary.job.0 == "j1").unwrap();
    let states: Vec<StepState> = j1.steps.iter().map(|s| s.state).collect();
    assert_eq!(states, [StepState::Failed, StepState::Skipped]);
    assert!(j1.steps[0].started_at.is_some());
    assert_eq!(j1.steps[1].started_at, None, "a skipped step never started");
    assert_eq!(j1.summary.timeout_s, Some(600));
    assert_eq!(j1.note.as_deref(), Some("why"));
    assert_eq!(j1.cwd.as_deref(), Some("/work/repo"));
    let j2 = all.iter().find(|d| d.summary.job.0 == "j2").unwrap();
    assert!(j2.cancelled_by.is_some(), "{j2:?}");

    let mut bob = h.local_hub("bob").await;
    let theirs = details(
        bob.read(ReadRequest::HistoryDetails(HistoryFilter::default()))
            .await,
    );
    let ids: Vec<&str> = theirs.iter().map(|d| d.summary.job.0.as_str()).collect();
    assert!(
        ids.contains(&"j1") && ids.contains(&"j2"),
        "bob sees the runs at summary: {ids:?}"
    );
    for d in &theirs {
        assert!(
            d.steps.is_empty() && d.note.is_none() && d.cwd.is_none(),
            "{d:?}"
        );
        assert!(
            d.cancelled_by.is_none() && d.summary.project.is_none(),
            "{d:?}"
        );
        assert!(d.summary.timeout_s.is_none() && d.summary.github_run_url.is_none());
    }
}

#[tokio::test]
async fn a_queued_order_carries_the_kind_of_its_wait_for_its_owner_only() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&order("j1", req(512, 1, 600))).await;
    alex.accepted().await;
    r.assign().await;
    alex.send(&order("j2", req(512, 1, 600))).await;
    alex.accepted().await;
    let queued = |reply: ReadReply| match reply {
        ReadReply::Status(s) => s.queue,
        other => panic!("expected status, got {other:?}"),
    };
    let mine = queued(
        alex.read(ReadRequest::Status(StatusFilter::default()))
            .await,
    );
    assert_eq!(
        mine[0].wait_code.as_deref(),
        Some("runner_busy"),
        "{mine:?}"
    );
    let mut bob = h.local_hub("bob").await;
    let theirs = queued(bob.read(ReadRequest::Status(StatusFilter::default())).await);
    assert_eq!(theirs.len(), 1, "bob sees the queued order at summary");
    assert!(theirs.iter().all(|q| q.wait_code.is_none()), "{theirs:?}");
}

#[tokio::test]
async fn step_statistics_count_the_steps_that_ran() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&two_steps("j1")).await;
    alex.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    alex.started_notice().await;
    h.clock.advance(Duration::from_secs(3));
    r.send(&step_result("j1", 2, 0, StepState::Passed, 2_000))
        .await;
    r.send(&step_result("j1", 3, 1, StepState::Passed, 1_000))
        .await;
    r.send(&result("j1", 4, JobState::Passed, 0)).await;
    r.acked(4).await;
    h.until_state("local:alex", "j1", JobState::Passed).await;
    let ReadReply::Stats(stats) = alex.read(ReadRequest::Stats(StatsFilter::default())).await
    else {
        panic!("stats")
    };
    let names: Vec<&str> = stats.profiles[0]
        .steps
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, ["build", "test"], "{stats:?}");
    assert_eq!(stats.profiles[0].steps[0].duration_ms.median, Some(2_000));
}

#[tokio::test]
async fn a_skipped_step_has_no_start_and_stays_out_of_step_statistics() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&two_steps("j1")).await;
    alex.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    alex.started_notice().await;
    h.clock.advance(Duration::from_secs(3));
    r.send(&step_result("j1", 2, 0, StepState::Failed, 2_000))
        .await;
    r.send(&step_result("j1", 3, 1, StepState::Skipped, 0))
        .await;
    r.send(&result("j1", 4, JobState::Failed, 1)).await;
    r.acked(4).await;
    h.until_state("local:alex", "j1", JobState::Failed).await;
    let ReadReply::Stats(stats) = alex.read(ReadRequest::Stats(StatsFilter::default())).await
    else {
        panic!("stats")
    };
    let steps: Vec<(&str, u64)> = stats.profiles[0]
        .steps
        .iter()
        .map(|s| (s.name.as_str(), s.runs))
        .collect();
    assert_eq!(steps, [("build", 1)], "{stats:?}");
}
