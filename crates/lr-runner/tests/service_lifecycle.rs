mod common;

use common::svc::*;
use common::*;
use lr_abstractions::fakes::{JobScript, StepScript};
use lr_abstractions::metric::{SERVICE_FAILURES_TOTAL, SERVICE_STARTS_TOTAL, SERVICE_UP};
use lr_abstractions::{Level, ServiceState};
use lr_core::{JobId, JobState, StepState};
use lr_proto::{ReasonCode, RunnerToBroker, WireServiceStatus};
use lr_runner::Lifetime;
use std::time::Duration;

fn skipped_steps(msgs: &[RunnerToBroker]) -> Vec<StepState> {
    msgs.iter()
        .filter_map(|m| match m {
            RunnerToBroker::StepResult(s) => Some(s.state),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_per_job_service_is_ready_before_the_steps_and_stopped_before_the_result() {
    let rig = rig_with(db(Lifetime::PerJob));
    rig.script("job-1", JobScript::new(vec![StepScript::new(10 * SEC, 0)]));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let caps = b.hello.capabilities.clone().unwrap();
    assert_eq!(caps.services.get("db"), Some(&WireServiceStatus::Available));
    let msgs = run(&rig, &b, "job-1", needs_db("client-1", "acme/app")).await;
    assert_eq!(result_of(&msgs).state, JobState::Passed);
    let spawns = rig.process.spawns();
    assert_eq!(spawns.len(), 1);
    assert_eq!(spawns[0].command, "db-start");
    assert_eq!(spawns[0].env, service_env(), "only the [env] whitelist");
    assert!(spawns[0].secrets.is_empty());
    assert_eq!(spawns[0].cwd, state_dir().join("services").join("db"));
    assert_eq!(commands(&rig), ["db-ready", "make", "db-stop"]);
    assert_eq!(rig.process.runs()[0].env, service_env());
    assert_eq!(
        rig.process.kills().len(),
        1,
        "the tree is killed after stop"
    );
    assert!(
        rig.logs_text().contains("did not stop"),
        "{}",
        rig.logs_text()
    );
    assert!(registry(&rig).await.is_empty());
    let seen = reports(&msgs);
    assert!(seen.iter().any(|r| r
        .iter()
        .any(|s| s.name == "db" && s.state == lr_proto::ServiceState::Busy && s.mem_mb == 512)));
    assert_eq!(
        seen.last(),
        Some(&Vec::new()),
        "the stop is reported before the result"
    );
    assert_eq!(rig.meter.gauge(SERVICE_UP, DB), Some(0.0));
    assert_eq!(rig.meter.counter(SERVICE_STARTS_TOTAL, DB), 1);
    assert_eq!(rig.meter.counter(SERVICE_FAILURES_TOTAL, DB), 0);
}

#[tokio::test]
async fn a_failed_readiness_check_fails_the_job_with_service_failed() {
    let rig = rig_with(db(Lifetime::PerJob));
    ready_fails(&rig);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let msgs = run(&rig, &b, "job-1", needs_db("client-1", "acme/app")).await;
    let r = result_of(&msgs);
    assert_eq!(r.state, JobState::Failed);
    assert_eq!(r.reason, Some(ReasonCode::ServiceFailed));
    assert!(r.detail.unwrap().contains("ready_check"));
    assert_eq!(skipped_steps(&msgs), [StepState::Skipped]);
    assert!(commands(&rig).iter().all(|c| c == "db-ready"));
    assert!(
        commands(&rig).len() > 1,
        "the check is repeated until the deadline"
    );
    assert_eq!(rig.process.kills().len(), 1);
    assert!(registry(&rig).await.is_empty());
    assert_eq!(rig.meter.counter(SERVICE_STARTS_TOTAL, DB), 1);
    assert_eq!(rig.meter.counter(SERVICE_FAILURES_TOTAL, DB), 1);
    assert_ne!(rig.meter.gauge(SERVICE_UP, DB), Some(1.0));
}

#[tokio::test]
async fn a_service_that_cannot_start_fails_the_job_at_once() {
    let rig = rig_with(db(Lifetime::PerJob));
    rig.process.refuse_spawn("db-start");
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let msgs = run(&rig, &b, "job-1", needs_db("client-1", "acme/app")).await;
    let r = result_of(&msgs);
    assert_eq!(r.reason, Some(ReasonCode::ServiceFailed));
    assert!(commands(&rig).is_empty(), "no check and no step ran");
    assert_eq!(rig.meter.counter(SERVICE_FAILURES_TOTAL, DB), 1);
}

#[tokio::test]
async fn failures_beyond_the_restart_limit_make_the_service_unavailable() {
    let rig = rig_with(db(Lifetime::PerJob));
    rig.process.refuse_spawn("db-start");
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let first = run(&rig, &b, "job-1", needs_db("c", "p")).await;
    assert_eq!(result_of(&first).reason, Some(ReasonCode::ServiceFailed));
    let second = run(&rig, &b, "job-2", needs_db("c", "p")).await;
    assert_eq!(result_of(&second).reason, Some(ReasonCode::ServiceFailed));
    assert_eq!(rig.process.spawns().len(), 2, "one restart allowed");
    let unavailable = |m: &RunnerToBroker| {
        status_of(std::slice::from_ref(m)) == Some(WireServiceStatus::Unavailable)
    };
    if !second.iter().any(unavailable) {
        b.until(unavailable).await;
    }
    let third = run(&rig, &b, "job-3", needs_db("c", "p")).await;
    let r = result_of(&third);
    assert_eq!(r.reason, Some(ReasonCode::ServiceFailed));
    assert!(r.detail.unwrap().contains("unavailable"));
    assert_eq!(rig.process.spawns().len(), 2, "no start once unavailable");
}

#[tokio::test]
async fn a_service_not_declared_on_the_runner_fails_the_job() {
    let rig = rig_with(db(Lifetime::PerJob));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let mut spec = needs_db("c", "p");
    spec.requirements.services = vec!["cache-server".into()];
    let r = result_of(&run(&rig, &b, "job-1", spec).await);
    assert_eq!(r.reason, Some(ReasonCode::ServiceFailed));
    assert!(r.detail.unwrap().contains("cache-server"));
    assert!(rig.process.spawns().is_empty());
}

#[tokio::test]
async fn a_service_gets_only_the_secrets_it_requests() {
    let mut spec = db(Lifetime::PerJob);
    spec.secrets = vec!["registry_auth".into()];
    let rig = rig_with(spec);
    rig.secrets.insert("registry_auth", "s3cr3t");
    rig.secrets.insert("other", "x");
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let msgs = run(&rig, &b, "job-1", needs_db("c", "p")).await;
    assert_eq!(result_of(&msgs).state, JobState::Passed);
    let spawn = &rig.process.spawns()[0];
    let names: Vec<&str> = spawn.secrets.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["REGISTRY_AUTH"]);
    assert_eq!(spawn.secrets[0].1.expose(), "s3cr3t");
    assert!(!rig.logs_text().contains("s3cr3t"));
}

#[tokio::test]
async fn a_missing_service_secret_fails_the_job_with_secret_missing() {
    let mut spec = db(Lifetime::PerJob);
    spec.secrets = vec!["registry_auth".into()];
    let rig = rig_with(spec);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let r = result_of(&run(&rig, &b, "job-1", needs_db("c", "p")).await);
    assert_eq!(r.reason, Some(ReasonCode::SecretMissing));
    assert!(rig.process.spawns().is_empty());
}

#[tokio::test]
async fn a_cancel_during_readiness_kills_the_starting_service() {
    let rig = rig_with(db(Lifetime::PerJob));
    rig.process.script(
        JobId("service:db:ready".into()),
        JobScript::new(vec![StepScript::new(Duration::from_secs(60), 0)]),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, needs_db("c", "p")).await;
    b.next_job_msg().await.unwrap();
    rig.advance(SEC).await;
    b.cancel("job-1", 2, ReasonCode::CancelledByUser).await;
    let msgs = drive(&rig, collect(&b)).await;
    let r = result_of(&msgs);
    assert_eq!(r.state, JobState::Cancelled);
    assert_eq!(r.reason, Some(ReasonCode::CancelledByUser));
    assert_eq!(rig.process.kills().len(), 1);
    assert!(registry(&rig).await.is_empty());
}

#[tokio::test]
async fn a_starting_service_is_recorded_so_a_crash_leaves_no_orphan() {
    let rig = rig_with(db(Lifetime::PerJob));
    rig.process.script(
        JobId("service:db:ready".into()),
        JobScript::new(vec![StepScript::new(Duration::from_secs(60), 0)]),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, needs_db("c", "p")).await;
    b.next_job_msg().await.unwrap();
    rig.settle().await;
    assert_eq!(
        registry(&rig).await,
        vec![("db".to_string(), ServiceState::Starting)]
    );
    assert!(rig
        .log
        .records()
        .iter()
        .any(|r| r.level == Level::Info && r.message.contains("service starting")));
}
