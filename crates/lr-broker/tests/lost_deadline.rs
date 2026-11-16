mod common;

use common::harness::*;
use lr_core::{JobId, JobState, Timestamp};
use lr_proto::*;
use std::time::Duration;

const ALEX: &str = "local:alex";
const GRACE: &str = "[grace]\nlost_min = 1\n";
const TIMEOUT_S: u64 = 10;
const DEADLINE_MS: u64 = (TIMEOUT_S + 60) * 1000;
const STEP_MS: u64 = 10_000;
const HANG: Duration = Duration::from_secs(5);

async fn keep_alive(h: &Harness, runner: &Conn, hub: &Conn, mut ms: u64) {
    while ms > 0 {
        let step = ms.min(STEP_MS);
        h.clock.advance(Duration::from_millis(step));
        runner.send(&RunnerToBroker::Heartbeat { load: None }).await;
        hub.send(&HubToBroker::Heartbeat).await;
        h.poke().await;
        ms -= step;
    }
}

#[tokio::test]
async fn a_killed_runner_loses_its_job_at_the_deadline_and_the_pool_takes_the_next() {
    let h = Harness::start(GRACE);
    let mut r1 = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("held", req(16384, 1, TIMEOUT_S))).await;
    hub.accepted().await;
    assert_eq!(r1.assign().await.job, JobId("held".into()));
    r1.send(&started("held", 1)).await;
    r1.acked(1).await;
    hub.started_notice().await;

    let mut r2 = h.member_runner("r2", "10.0.0.8", linux()).await;
    hub.send(&order("quick", req(16384, 1, 60))).await;
    hub.accepted().await;
    r2.quiet().await;
    assert_eq!(
        h.job_row(ALEX, "quick").await.unwrap().job.state,
        JobState::Queued,
        "the quick job waits for the pool"
    );

    r1.close().await;
    hub.until_runner("r1", false).await;
    keep_alive(&h, &r2, &hub, DEADLINE_MS - 1).await;
    assert_eq!(
        h.job_row(ALEX, "held").await.unwrap().job.state,
        JobState::Running,
        "not lost before timeout_s + lost_min"
    );
    r2.quiet().await;
    hub.until_runner("r2", true).await;

    let mut events = h.events.resubscribe();
    keep_alive(&h, &r2, &hub, 1).await;
    let finished = tokio::time::timeout(HANG, async {
        loop {
            if let QueueEvent::JobFinished { job, state, reason } =
                events.recv().await.expect("broker bus").event
            {
                if job == JobId("held".into()) {
                    return (state, reason);
                }
            }
        }
    })
    .await
    .expect("the held job finishes");
    assert_eq!(finished, (JobState::Lost, Some(ReasonCode::RunnerLost)));
    let row = h.job_row(ALEX, "held").await.expect("held row");
    let started_at = row.job.started_at.expect("it was started");
    assert_eq!(
        row.job.finished_at,
        Some(Timestamp(started_at.0 + DEADLINE_MS))
    );
    assert_eq!(row.job.cancel_reason.as_deref(), Some("runner_lost"));
    let lost = loop {
        if let BrokerToHub::JobResult(r) = hub.hub().await {
            if r.job == JobId("held".into()) {
                break r;
            }
        }
    };
    assert_eq!(
        (lost.state, lost.reason),
        (JobState::Lost, Some(ReasonCode::RunnerLost))
    );
    assert_eq!(
        r2.assign().await.job,
        JobId("quick".into()),
        "the pool memory came back"
    );
}
