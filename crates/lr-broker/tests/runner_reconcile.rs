mod common;

use common::harness::*;
use lr_core::{JobId, JobState};
use lr_proto::*;

const ALEX: &str = "local:alex";

async fn reconnect(h: &Harness, name: &str, addr: &str, points: Vec<ResumePoint>) -> Conn {
    let mut c = h.dial(member_peer(&format!("pk-{name}"), addr)).await;
    c.send(&hello(name, Some(linux()))).await;
    c.welcome().await;
    c.send(&RunnerToBroker::Resume(Resume { points })).await;
    c
}

async fn running_job(hub: &mut Conn, runner: &mut Conn, id: &str) {
    hub.send(&order(id, req(16384, 1, 600))).await;
    hub.accepted().await;
    assert_eq!(runner.assign().await.job, JobId(id.into()));
    runner.send(&started(id, 1)).await;
    runner.acked(1).await;
    hub.started_notice().await;
}

#[tokio::test]
async fn a_runner_that_reconnects_without_a_running_job_loses_it_and_frees_the_pool() {
    let h = Harness::start("");
    let mut r1 = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    running_job(&mut hub, &mut r1, "gone").await;
    hub.send(&order("next", req(16384, 1, 600))).await;
    hub.accepted().await;
    r1.quiet().await;
    assert_eq!(
        h.job_row(ALEX, "next").await.unwrap().job.state,
        JobState::Queued
    );

    r1.close().await;
    let mut r1 = reconnect(&h, "r1", "10.0.0.7", Vec::new()).await;
    assert_eq!(r1.assign().await.job, JobId("next".into()));
    let row = h.job_row(ALEX, "gone").await.unwrap();
    assert_eq!(row.job.state, JobState::Lost);
    assert_eq!(row.job.cancel_reason.as_deref(), Some("runner_lost"));
}

#[tokio::test]
async fn a_cancelling_job_the_runner_no_longer_knows_is_lost_too() {
    let h = Harness::start("");
    let mut r1 = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    running_job(&mut hub, &mut r1, "stuck").await;
    hub.send(&cancel("stuck", 1)).await;
    r1.close().await;
    let _r1 = reconnect(&h, "r1", "10.0.0.7", Vec::new()).await;
    h.until_state(ALEX, "stuck", JobState::Lost).await;
}

#[tokio::test]
async fn a_job_the_runner_still_reports_is_kept() {
    let h = Harness::start("");
    let mut r1 = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    running_job(&mut hub, &mut r1, "alive").await;
    r1.close().await;
    let point = ResumePoint {
        job: JobId("alive".into()),
        last_seq: 1,
    };
    let mut r1 = reconnect(&h, "r1", "10.0.0.7", vec![point]).await;
    r1.quiet().await;
    assert_eq!(
        h.job_row(ALEX, "alive").await.unwrap().job.state,
        JobState::Running
    );
}
