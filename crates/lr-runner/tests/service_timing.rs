mod common;

use common::svc::*;
use common::*;
use lr_abstractions::{Clock, RunnerStore, ServiceRow, ServiceState};
use lr_core::{JobState, Timestamp};
use lr_proto::{BrokerToRunner, ReasonCode, RunnerToBroker};
use lr_runner::Lifetime;

#[tokio::test]
async fn ready_check_runs_once_a_second_until_ready_timeout() {
    let mut spec = db(Lifetime::PerJob);
    spec.ready_timeout_s = 8;
    let rig = rig_with(spec);
    ready_fails(&rig);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let msgs = run(&rig, &b, "job-1", needs_db("c", "p")).await;
    let result = result_of(&msgs);
    assert_eq!(result.state, JobState::Failed);
    assert_eq!(result.reason, Some(ReasonCode::ServiceFailed));
    let checks = commands(&rig).iter().filter(|c| *c == "db-ready").count();
    assert_eq!(checks, 8, "{:?}", commands(&rig));
}

async fn stuck_service_row(rig: &Rig) {
    rig.store
        .upsert_service(ServiceRow {
            name: "db".into(),
            pid: 0,
            pid_start: Some(1),
            state: ServiceState::Idle,
            started_at: Timestamp(START),
            expires_at: Timestamp(START + 3_600_000),
            mem_mb: 512,
        })
        .await
        .unwrap();
}

async fn seconds_from_drain_to_bye(stop_timeout_s: u64) -> u64 {
    let mut spec = db(Lifetime::Keep);
    spec.stop_timeout_s = stop_timeout_s;
    let rig = rig_with(spec);
    stuck_service_row(&rig).await;
    let (c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    rig.settle().await;
    let from = rig.clock.now();
    c.drain().await;
    let bye = b.until(|m| matches!(m, RunnerToBroker::Bye(_)));
    tokio::pin!(bye);
    let mut ticks = 0u64;
    loop {
        tokio::select! {
            biased;
            _ = &mut bye => break,
            _ = rig.advance(SEC) => {
                ticks += 1;
                assert!(ticks < 500, "no bye: {}", rig.logs_text());
                if ticks % 10 == 0 {
                    b.send(BrokerToRunner::Heartbeat).await;
                }
            }
        }
    }
    assert!(
        rig.logs_text()
            .contains("services still stopping, runner stops"),
        "{}",
        rig.logs_text()
    );
    assert_eq!(
        rig.store.services().await.unwrap().len(),
        1,
        "the service that will not go is still registered"
    );
    rig.clock.now().millis_since(from) / 1000
}

#[tokio::test]
async fn bye_waits_for_services_at_most_the_longest_stop_timeout_plus_30_s() {
    assert_eq!(seconds_from_drain_to_bye(3).await, 33);
    assert_eq!(seconds_from_drain_to_bye(40).await, 70);
}
