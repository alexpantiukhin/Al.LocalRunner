mod common;

use common::harness::*;
use lr_abstractions::BrokerStore;
use lr_core::{JobId, JobState, MachineRef, StepState, Timestamp};
use lr_proto::*;
use std::time::Duration;

const ALEX: &str = "local:alex";

#[tokio::test]
async fn hole_filling_respects_head_reservation() {
    let h = Harness::start("");
    let mut r1 = h
        .member_runner("r1", "10.0.0.7", caps(&["linux"], 16384, 8))
        .await;
    let mut r2 = h
        .member_runner("r2", "10.0.0.8", caps(&["linux"], 16000, 8))
        .await;
    let mut r3 = h
        .member_runner("r3", "10.0.0.9", caps(&["linux"], 15000, 8))
        .await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order_at("a", req(512, 6, 600), START)).await;
    hub.accepted().await;
    assert_eq!(r1.assign().await.job, JobId("a".into()));
    r1.send(&started("a", 1)).await;
    hub.send(&order_at("head", req(512, 8, 60), START + 1))
        .await;
    let head = hub.accepted().await;
    assert!(head.waiting.is_some());
    hub.send(&order_at("short", req(512, 1, 100), START + 2))
        .await;
    assert_eq!(hub.accepted().await.waiting, None);
    assert_eq!(r2.assign().await.job, JobId("short".into()));
    hub.send(&order_at("long", req(512, 1, 1000), START + 3))
        .await;
    let long = hub.accepted().await;
    let reason = long.waiting.expect("long waits");
    assert!(
        reason.contains("зарезервирован") && reason.contains("head"),
        "{reason}"
    );
    r3.quiet().await;
}

#[tokio::test]
async fn head_starts_when_running_job_ends_early() {
    let h = Harness::start("");
    let mut r1 = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut r2 = h
        .member_runner("r2", "10.0.0.8", caps(&["linux"], 8000, 8))
        .await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order_at("a", req(512, 6, 600), START)).await;
    hub.accepted().await;
    r1.assign().await;
    r1.send(&started("a", 1)).await;
    hub.send(&order_at("head", req(512, 8, 60), START + 1))
        .await;
    hub.accepted().await;
    h.clock.advance(Duration::from_secs(10));
    r1.send(&result("a", 2, JobState::Passed, 0)).await;
    assert_eq!(r1.assign().await.job, JobId("head".into()));
    r2.quiet().await;
}

#[tokio::test]
async fn started_is_stamped_with_broker_clock() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    h.clock.advance(Duration::from_secs(5));
    r.send(&started("j1", 1)).await;
    let notice = loop {
        if let BrokerToHub::Started(s) = hub.hub().await {
            break s;
        }
    };
    assert_eq!(notice.started_at, Timestamp(START + 5000));
    assert_eq!(notice.runner, "r1");
    assert_eq!(notice.seq, 1);
    let row = h.job_row(ALEX, "j1").await.unwrap().job;
    assert_eq!(row.state, JobState::Running);
    assert_eq!(row.started_at, Some(Timestamp(START + 5000)));
}

#[tokio::test]
async fn result_writes_final_state_releases_runner_and_next_order_is_assigned() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    hub.send(&order("j2", req(512, 1, 60))).await;
    hub.accepted().await;
    r.send(&started("j1", 1)).await;
    h.clock.advance(Duration::from_secs(3));
    r.send(&result("j1", 2, JobState::Failed, 3)).await;
    assert_eq!(r.assign().await.job, JobId("j2".into()));
    let res = loop {
        if let BrokerToHub::JobResult(x) = hub.hub().await {
            break x;
        }
    };
    assert_eq!(res.state, JobState::Failed);
    assert_eq!(res.exit_code, Some(3));
    let row = h.job_row(ALEX, "j1").await.unwrap().job;
    assert_eq!(row.state, JobState::Failed);
    assert_eq!(row.exit_code, Some(3));
    assert_eq!(row.finished_at, Some(Timestamp(START + 3000)));
}

#[tokio::test]
async fn higher_priority_order_is_assigned_first() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    let mut ci = h.member_hub("ci-box", "ci", "10.0.0.20").await;
    alex.send(&order_at("j0", req(512, 1, 60), START)).await;
    alex.accepted().await;
    r.assign().await;
    alex.send(&order_at("a1", req(512, 1, 60), START + 1)).await;
    alex.accepted().await;
    ci.send(&order_at("c1", req(512, 1, 60), START + 2)).await;
    ci.accepted().await;
    r.send(&result("j0", 1, JobState::Passed, 0)).await;
    let next = r.assign().await;
    assert_eq!(next.job, JobId("c1".into()));
    assert_eq!(next.spec.client, "ci-box");
}

#[tokio::test]
async fn running_job_is_never_preempted_by_higher_priority() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    let mut ci = h.member_hub("ci-box", "ci", "10.0.0.20").await;
    alex.send(&order("a1", req(512, 1, 60))).await;
    alex.accepted().await;
    r.assign().await;
    r.send(&started("a1", 1)).await;
    ci.send(&order("c1", req(512, 1, 60))).await;
    assert_eq!(ci.accepted().await.waiting.as_deref(), Some("раннер занят"));
    alex.sync().await;
    match r.runner().await {
        BrokerToRunner::Ack(a) => assert_eq!(a.seq, 1),
        other => panic!("unexpected {other:?}"),
    }
    r.quiet().await;
    assert_eq!(
        h.job_row(ALEX, "a1").await.unwrap().job.state,
        JobState::Running
    );
}

#[tokio::test]
async fn runner_nearer_to_client_is_preferred_but_not_reserved() {
    let h = Harness::start("");
    let mut far = h
        .member_runner("r-far", "10.0.0.7", caps(&["linux"], 65536, 32))
        .await;
    let mut near = h.local_runner("alex", "r-near", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    assert_eq!(near.assign().await.job, JobId("j1".into()));
    hub.send(&order("j2", req(512, 1, 60))).await;
    assert_eq!(hub.accepted().await.waiting, None);
    assert_eq!(far.assign().await.job, JobId("j2".into()));
}

#[tokio::test]
async fn same_machine_order_goes_only_to_ipc_runner_of_the_same_os_user() {
    let h = Harness::start("");
    let mut forged = caps(&["linux"], 16384, 8);
    forged.machine = Some(MachineRef(ALEX.into()));
    let mut fake = h.member_runner("forged", "10.0.0.7", forged).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", same_machine_req())).await;
    let r = hub.rejected().await;
    assert_eq!(
        r.code,
        ReasonCode::Rejected(RejectReason::NoRunnerOnMachine)
    );
    let mut bobs = h.local_runner("bob", "r-bob", linux()).await;
    let mut mine = h.local_runner("alex", "r-alex", linux()).await;
    hub.send(&order("j2", same_machine_req())).await;
    hub.accepted().await;
    let a = mine.assign().await;
    assert_eq!(a.job, JobId("j2".into()));
    assert_eq!(
        a.spec.requirements.same_machine,
        Some(MachineRef(ALEX.into()))
    );
    fake.quiet().await;
    bobs.quiet().await;
}

#[tokio::test]
async fn chunk_is_logged_and_relayed_to_the_hub() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    r.send(&chunk("j1", 2, "hello\n")).await;
    let c = loop {
        if let BrokerToHub::Chunk(c) = hub.hub().await {
            break c;
        }
    };
    assert_eq!(c.text, "hello\n");
    assert_eq!(c.seq, 2);
    let log = h
        .store
        .read_log(&lr_abstractions::JobKey::new(ALEX, "j1"))
        .await
        .unwrap();
    assert_eq!(log, b"hello\n");
}

#[tokio::test]
async fn step_result_updates_the_step_and_is_relayed() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    r.send(&RunnerToBroker::StepResult(StepResult {
        job: JobId("j1".into()),
        seq: 2,
        ord: 0,
        name: "build".into(),
        state: StepState::Passed,
        exit_code: Some(0),
        duration_ms: 10,
    }))
    .await;
    let s = loop {
        if let BrokerToHub::StepResult(s) = hub.hub().await {
            break s;
        }
    };
    assert_eq!(s.state, StepState::Passed);
    let step = &h.job_row(ALEX, "j1").await.unwrap().steps[0];
    assert_eq!(step.state, StepState::Passed);
    assert_eq!(step.exit_code, Some(0));
}

#[tokio::test]
async fn runner_gone_before_start_returns_the_order_to_the_queue() {
    let h = Harness::start("");
    let a = h.member_runner("rA", "10.0.0.7", linux()).await;
    let mut b = h
        .member_runner("rB", "10.0.0.8", caps(&["linux"], 8192, 8))
        .await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    a.close().await;
    let again = b.assign().await;
    assert_eq!(again.job, JobId("j1".into()));
    assert_eq!(again.seq, 1);
    let row = h.job_row(ALEX, "j1").await.unwrap().job;
    assert_eq!(row.runner.as_deref(), Some("rB"));
    assert_eq!(row.state, JobState::Queued);
}

#[tokio::test]
async fn late_result_of_a_runner_no_longer_assigned_is_ignored() {
    let h = Harness::start("");
    let a = h.member_runner("rA", "10.0.0.7", linux()).await;
    let mut b = h
        .member_runner("rB", "10.0.0.8", caps(&["linux"], 8192, 8))
        .await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    a.close().await;
    b.assign().await;
    let mut back = h.dial(member_peer("pk-rA", "10.0.0.7")).await;
    back.send(&hello("rA", Some(linux()))).await;
    back.welcome().await;
    back.send(&result("j1", 1, JobState::Passed, 0)).await;
    match back.runner().await {
        BrokerToRunner::Ack(x) => assert_eq!(x.seq, 1),
        other => panic!("expected ack, got {other:?}"),
    }
    let row = h.job_row(ALEX, "j1").await.unwrap().job;
    assert_eq!(row.state, JobState::Queued);
    assert_eq!(row.runner.as_deref(), Some("rB"));
}

#[tokio::test]
async fn project_cache_share_sends_one_cache_client_for_two_clients_of_a_project() {
    let h = Harness::start("[cache]\nshare = \"project\"\n");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut laptop = h.member_hub("laptop", "agent", "10.0.0.20").await;
    let mut desk = h.member_hub("desk", "agent", "10.0.0.21").await;
    laptop.send(&order("j1", req(512, 1, 60))).await;
    laptop.accepted().await;
    let first = r.assign().await;
    r.send(&started("j1", 1)).await;
    r.send(&result("j1", 2, JobState::Passed, 0)).await;
    desk.send(&order("j2", req(512, 1, 60))).await;
    desk.accepted().await;
    let second = r.assign().await;
    assert_ne!(first.spec.client, second.spec.client);
    assert_eq!(first.spec.project, second.spec.project);
    assert_eq!(
        first.spec.cache_client.as_deref(),
        Some(SHARED_CACHE_CLIENT)
    );
    assert_eq!(second.spec.cache_client, first.spec.cache_client);
}

#[tokio::test]
async fn default_cache_share_keeps_the_cache_of_each_client() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut laptop = h.member_hub("laptop", "agent", "10.0.0.20").await;
    laptop.send(&order("j1", req(512, 1, 60))).await;
    laptop.accepted().await;
    let a = r.assign().await;
    assert_eq!(a.spec.client, "laptop");
    assert_eq!(a.spec.cache_client, None);
}

#[tokio::test]
async fn ci_order_takes_the_pool_budget_once() {
    let h = Harness::start("");
    let mut ci_runner = h
        .member_runner("al-ci", "10.0.0.21", caps(&["linux"], 16384, 8))
        .await;
    let mut r2 = h
        .member_runner("r2", "10.0.0.7", caps(&["linux"], 8000, 8))
        .await;
    let mut r3 = h
        .member_runner("r3", "10.0.0.8", caps(&["linux"], 8000, 8))
        .await;
    let mut ci = h.member_hub("ci-box", "ci", "10.0.0.21").await;
    ci.send(&order("ci-1", req(10000, 5, 600))).await;
    assert_eq!(ci.accepted().await.waiting, None);
    assert_eq!(ci_runner.assign().await.job, JobId("ci-1".into()));
    ci_runner.send(&started("ci-1", 1)).await;
    let mut agent = h.member_hub("laptop", "agent", "10.0.0.20").await;
    agent.send(&order("rest", req(6384, 3, 600))).await;
    assert_eq!(agent.accepted().await.waiting, None);
    assert_eq!(r2.assign().await.job, JobId("rest".into()));
    agent.send(&order("over", req(512, 1, 600))).await;
    let over = agent.accepted().await;
    assert!(
        over.waiting.is_some(),
        "the pool is full: 16384 MB and 8 CPU"
    );
    r3.quiet().await;
}

#[tokio::test]
async fn the_cache_share_is_taken_from_the_policy_at_assignment() {
    let h = Harness::start("");
    let r = h.member_runner("r1", "10.0.0.7", linux()).await;
    r.close().await;
    let mut laptop = h.member_hub("laptop", "agent", "10.0.0.20").await;
    laptop.until_runner("r1", false).await;
    laptop.send(&order("j1", req(512, 1, 60))).await;
    laptop.accepted().await;
    h.handle
        .reload_policy(&policy("[cache]\nshare = \"project\"\n"))
        .await
        .expect("reload applies");
    let mut back = h.dial(member_peer("pk-r1", "10.0.0.7")).await;
    back.send(&hello("r1", Some(linux()))).await;
    back.welcome().await;
    let a = back.assign().await;
    assert_eq!(a.spec.cache_client.as_deref(), Some(SHARED_CACHE_CLIENT));
}
