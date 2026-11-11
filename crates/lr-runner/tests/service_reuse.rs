mod common;

use common::svc::*;
use common::*;
use lr_abstractions::fakes::{JobScript, StepScript};
use lr_abstractions::metric::{
    RUNNER, SERVICE, SERVICE_FAILURES_TOTAL, SERVICE_STARTS_TOTAL, SERVICE_UP,
};
use lr_abstractions::{RunnerStore, ServiceState};
use lr_core::JobState;
use lr_proto::{BrokerToRunner, RunnerToBroker, StopService};
use lr_runner::{AgentExit, Lifetime, Reuse};

fn idle_db() -> lr_runner::ServiceSpec {
    db(Lifetime::Idle)
}

async fn until_gone(rig: &Rig) {
    drive(rig, async {
        while !registry(rig).await.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

#[tokio::test]
async fn an_idle_service_serves_the_same_client_again_and_ends_after_its_idle_timeout() {
    let rig = rig_with(idle_db());
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    run(&rig, &b, "job-1", needs_db("client-1", "acme/app")).await;
    assert_eq!(
        registry(&rig).await,
        [("db".to_string(), ServiceState::Idle)]
    );
    assert_eq!(rig.meter.gauge(SERVICE_UP, DB), Some(1.0));
    let msgs = run(&rig, &b, "job-2", needs_db("client-1", "acme/app")).await;
    assert_eq!(result_of(&msgs).state, JobState::Passed);
    assert_eq!(rig.process.spawns().len(), 1, "the warm service is reused");
    assert_eq!(commands(&rig), ["db-ready", "make", "make"]);
    assert!(rig.process.kills().is_empty());
    until_gone(&rig).await;
    assert_eq!(rig.process.kills().len(), 1);
    assert_eq!(commands(&rig).last().map(String::as_str), Some("db-stop"));
    assert_eq!(rig.meter.gauge(SERVICE_UP, DB), Some(0.0));
}

#[tokio::test]
async fn another_client_never_gets_the_warm_service_of_a_client() {
    let rig = rig_with(idle_db());
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    run(&rig, &b, "job-1", needs_db("client-1", "acme/app")).await;
    let first = rig.process.spawns().len();
    run(&rig, &b, "job-2", needs_db("client-2", "acme/app")).await;
    assert_eq!(first, 1);
    assert_eq!(
        rig.process.spawns().len(),
        2,
        "a fresh service for client-2"
    );
    assert_eq!(
        rig.process.kills().len(),
        1,
        "the warm one was stopped first"
    );
    assert_eq!(rig.meter.counter(SERVICE_STARTS_TOTAL, DB), 2);
}

#[tokio::test]
async fn same_project_reuse_crosses_clients_but_not_projects() {
    let mut spec = idle_db();
    spec.reuse = Reuse::SameProject;
    let rig = rig_with(spec);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    run(&rig, &b, "job-1", needs_db("client-1", "acme/app")).await;
    run(&rig, &b, "job-2", needs_db("client-2", "acme/app")).await;
    assert_eq!(rig.process.spawns().len(), 1);
    run(&rig, &b, "job-3", needs_db("client-2", "acme/other")).await;
    assert_eq!(rig.process.spawns().len(), 2);
}

#[tokio::test]
async fn a_kept_service_ends_at_its_max_lifetime() {
    let mut spec = db(Lifetime::Keep);
    spec.max_lifetime_s = 40;
    let rig = rig_with(spec);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    run(&rig, &b, "job-1", needs_db("c", "p")).await;
    assert_eq!(registry(&rig).await.len(), 1);
    until_gone(&rig).await;
    assert_eq!(rig.process.kills().len(), 1);
}

#[tokio::test]
async fn a_service_past_its_max_lifetime_is_stopped_when_its_job_ends() {
    let mut spec = db(Lifetime::Keep);
    spec.max_lifetime_s = 5;
    let rig = rig_with(spec);
    rig.script("job-1", JobScript::new(vec![StepScript::new(10 * SEC, 0)]));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let msgs = run(&rig, &b, "job-1", needs_db("c", "p")).await;
    assert_eq!(result_of(&msgs).state, JobState::Passed);
    assert!(registry(&rig).await.is_empty());
    assert_eq!(rig.process.kills().len(), 1);
}

#[tokio::test]
async fn eviction_stops_an_idle_service_but_never_one_serving_a_job() {
    let rig = rig_with(idle_db());
    rig.script("job-1", JobScript::new(vec![StepScript::new(30 * SEC, 0)]));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, needs_db("c", "p")).await;
    drive(&rig, async {
        while registry(&rig).await != [("db".to_string(), ServiceState::Busy)] {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let evict = BrokerToRunner::StopService(StopService { name: "db".into() });
    b.send(evict.clone()).await;
    let msgs = drive(&rig, collect(&b)).await;
    assert_eq!(result_of(&msgs).state, JobState::Passed);
    assert!(rig.process.kills().is_empty(), "the busy service survived");
    assert_eq!(
        registry(&rig).await,
        [("db".to_string(), ServiceState::Idle)]
    );
    b.send(evict).await;
    until_gone(&rig).await;
    assert_eq!(rig.process.kills().len(), 1);
    assert_eq!(commands(&rig).last().map(String::as_str), Some("db-stop"));
}

#[tokio::test]
async fn a_crashed_warm_service_counts_as_a_failure_and_is_restarted() {
    let rig = rig_with(idle_db());
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    run(&rig, &b, "job-1", needs_db("c", "p")).await;
    let pid = rig.process.spawns().len();
    let row = rig.store.services().await.unwrap().remove(0);
    rig.process.processes().remove(row.pid);
    let msgs = run(&rig, &b, "job-2", needs_db("c", "p")).await;
    assert_eq!(result_of(&msgs).state, JobState::Passed);
    assert_eq!(pid, 1);
    assert_eq!(rig.process.spawns().len(), 2);
    assert_eq!(rig.meter.counter(SERVICE_FAILURES_TOTAL, DB), 1);
}

#[tokio::test]
async fn drain_stops_warm_services_before_bye() {
    let rig = rig_with(db(Lifetime::Keep));
    let (c, h) = rig.start();
    let b = Broker::accept(&rig).await;
    run(&rig, &b, "job-1", needs_db("c", "p")).await;
    assert_eq!(registry(&rig).await.len(), 1);
    c.drain().await;
    drive(&rig, b.until(|m| matches!(m, RunnerToBroker::Bye(_)))).await;
    assert!(registry(&rig).await.is_empty());
    assert_eq!(rig.process.kills().len(), 1);
    assert!(matches!(h.await.unwrap(), AgentExit::Stopped));
}

#[tokio::test]
async fn metrics_of_a_service_no_longer_declared_are_forgotten_at_start() {
    let rig = rig_with(idle_db());
    let row = |name: &str| lr_abstractions::ServiceRow {
        name: name.into(),
        pid: 4242,
        pid_start: Some(1),
        state: ServiceState::Idle,
        started_at: lr_core::Timestamp(START),
        expires_at: lr_core::Timestamp(START + 60_000),
        mem_mb: 1,
    };
    rig.store.upsert_service(row("old")).await.unwrap();
    rig.store.upsert_service(row("db")).await.unwrap();
    let up = lr_abstractions::metric::SERVICE_UP;
    let mine_old = [(RUNNER, "runner-a"), (SERVICE, "old")];
    let other_old = [(RUNNER, "runner-b"), (SERVICE, "old")];
    for labels in [&mine_old[..], &other_old[..], DB] {
        lr_abstractions::Meter::set(rig.meter.as_ref(), up, labels, 1.0);
    }
    let (_c, _h) = rig.start();
    let _b = Broker::accept(&rig).await;
    assert_eq!(
        rig.meter.gauge(up, &mine_old),
        None,
        "this runner's series is gone"
    );
    assert_eq!(
        rig.meter.gauge(up, &other_old),
        Some(1.0),
        "another runner keeps its own"
    );
    assert_eq!(
        rig.meter.gauge(up, DB),
        Some(1.0),
        "a declared service keeps its series"
    );
}
