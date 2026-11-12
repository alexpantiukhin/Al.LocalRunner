mod common;

use common::svc::*;
use common::*;
use lr_abstractions::{RunnerStore, ServiceState};
use lr_proto::{BrokerToRunner, ProtocolError, ProtocolErrorCode, RunnerToBroker, StopService};
use lr_runner::{AgentExit, Lifetime};
use std::path::Path;

async fn until_state(rig: &Rig, want: ServiceState) {
    drive(rig, async {
        while !registry(rig).await.iter().any(|(_, s)| *s == want) {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

#[tokio::test]
async fn bye_waits_until_a_stop_already_running_has_killed_the_tree() {
    let rig = rig_with(db(Lifetime::Keep));
    let (c, h) = rig.start();
    let b = Broker::accept(&rig).await;
    run(&rig, &b, "job-1", needs_db("c", "p")).await;
    b.send(BrokerToRunner::StopService(StopService {
        name: "db".into(),
    }))
    .await;
    until_state(&rig, ServiceState::Stopping).await;
    c.drain().await;
    let bye = drive(&rig, b.until(|m| matches!(m, RunnerToBroker::Bye(_)))).await;
    assert!(matches!(bye, RunnerToBroker::Bye(_)));
    assert_eq!(
        rig.process.kills().len(),
        1,
        "the tree is killed before bye"
    );
    assert!(registry(&rig).await.is_empty());
    assert!(matches!(h.await.unwrap(), AgentExit::Stopped));
}

#[tokio::test]
async fn heartbeats_go_on_while_services_stop_for_a_drain() {
    let mut spec = db(Lifetime::Keep);
    spec.stop_timeout_s = 40;
    let rig = rig_with(spec);
    let (c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    run(&rig, &b, "job-1", needs_db("c", "p")).await;
    c.drain().await;
    b.until(|m| matches!(m, RunnerToBroker::Drain)).await;
    until_state(&rig, ServiceState::Stopping).await;
    let mut beats = 0;
    let bye = drive(&rig, async {
        loop {
            match b.recv().await.expect("connection closed") {
                RunnerToBroker::Heartbeat { .. } => beats += 1,
                m @ RunnerToBroker::Bye(_) => return m,
                _ => {}
            }
        }
    })
    .await;
    assert!(matches!(bye, RunnerToBroker::Bye(_)));
    assert_eq!(rig.process.kills().len(), 1);
    assert!(beats >= 2, "heartbeats during a 40 s stop: {beats}");
}

#[tokio::test]
async fn an_authorization_refusal_stops_the_warm_services() {
    let rig = rig_with(db(Lifetime::Keep));
    let (_c, h) = rig.start();
    let b = Broker::accept(&rig).await;
    run(&rig, &b, "job-1", needs_db("c", "p")).await;
    assert_eq!(registry(&rig).await.len(), 1);
    b.send(BrokerToRunner::Error(ProtocolError {
        code: ProtocolErrorCode::KeyRevoked,
        message: "revoked".into(),
        supported: None,
    }))
    .await;
    let exit = drive(&rig, h).await.unwrap();
    assert!(matches!(exit, AgentExit::Refused(_)));
    assert_eq!(rig.process.kills().len(), 1);
    assert!(registry(&rig).await.is_empty());
}

#[tokio::test]
async fn a_service_that_ends_on_stop_is_not_reported_as_hung() {
    let rig = rig_with(db(Lifetime::Keep));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    run(&rig, &b, "job-1", needs_db("c", "p")).await;
    let row = rig.store.services().await.unwrap().remove(0);
    b.send(BrokerToRunner::StopService(StopService {
        name: "db".into(),
    }))
    .await;
    until_state(&rig, ServiceState::Stopping).await;
    rig.process.processes().remove(row.pid);
    drive(&rig, async {
        while !registry(&rig).await.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        !rig.logs_text().contains("did not stop"),
        "{}",
        rig.logs_text()
    );
    assert_eq!(
        rig.process.kills().len(),
        1,
        "the rest of the tree is still killed"
    );
    assert_eq!(commands(&rig).last().map(String::as_str), Some("db-stop"));
}

#[tokio::test]
async fn a_fresh_instance_starts_in_an_empty_directory() {
    let rig = rig_with(db(Lifetime::Idle));
    let left = state_dir()
        .join("services")
        .join("db")
        .join("client-1.state");
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    run(&rig, &b, "job-1", needs_db("client-1", "p")).await;
    rig.files.put(&left, b"private");
    run(&rig, &b, "job-2", needs_db("client-2", "p")).await;
    assert_eq!(rig.process.spawns().len(), 2);
    assert_eq!(rig.files.file(&left), None, "nothing of client-1 is left");
    assert!(rig
        .files
        .is_dir(Path::new(&state_dir().join("services").join("db"))));
}
