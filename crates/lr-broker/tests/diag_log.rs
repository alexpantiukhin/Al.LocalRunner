mod common;

use common::harness::*;
use lr_abstractions::{Level, Record, Role};
use lr_core::JobState;
use lr_proto::*;

fn field<'a>(r: &'a Record, name: &str) -> Option<&'a str> {
    r.fields
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn with_message(h: &Harness, message: &str) -> Vec<Record> {
    h.log
        .records()
        .into_iter()
        .filter(|r| r.message == message)
        .collect()
}

fn one(h: &Harness, message: &str, job: &str) -> Record {
    let found: Vec<Record> = with_message(h, message)
        .into_iter()
        .filter(|r| field(r, "job") == Some(job))
        .collect();
    assert_eq!(found.len(), 1, "{message} for {job}: {found:?}");
    found.into_iter().next().unwrap()
}

fn with_command(o: HubToBroker, command: &str) -> HubToBroker {
    let HubToBroker::Order(mut o) = o else {
        unreachable!()
    };
    o.spec.steps[0].command = command.into();
    HubToBroker::Order(o)
}

#[tokio::test]
async fn orders_and_connections_are_logged_with_job_client_and_runner() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.member_hub("hub1", "team", "10.0.0.5").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    hub.send(&order("big", req(99_999, 1, 60))).await;
    hub.rejected().await;

    let runner = with_message(&h, "runner connected");
    assert_eq!(runner.len(), 1);
    assert_eq!(field(&runner[0], "runner"), Some("r1"));
    assert_eq!(runner[0].role, Role::Broker);
    let client = with_message(&h, "client connected");
    assert!(
        client.iter().any(|r| field(r, "client") == Some("team")),
        "{client:?}"
    );

    let accepted = one(&h, "order accepted", "j1");
    assert_eq!(accepted.level, Level::Info);
    assert_eq!(field(&accepted, "client"), Some("team"));
    let issued = one(&h, "order issued to a runner", "j1");
    assert_eq!(field(&issued, "runner"), Some("r1"));
    assert_eq!(field(&issued, "client"), Some("team"));
    let rejected = one(&h, "order rejected", "big");
    assert_eq!(field(&rejected, "reason"), Some("rejected_pool_memory"));
    assert_eq!(field(&rejected, "client"), Some("team"));
    assert!(with_message(&h, "order accepted")
        .iter()
        .all(|r| field(r, "job") != Some("big")));

    r.close().await;
    hub.until_runner("r1", false).await;
    let gone = with_message(&h, "runner disconnected");
    assert_eq!(gone.len(), 1);
    assert_eq!(field(&gone[0], "runner"), Some("r1"));
}

#[tokio::test]
async fn a_refused_connection_is_logged_with_its_code() {
    let h = Harness::start("");
    let mut c = h.dial(member_peer("pk-nobody", "10.0.0.9")).await;
    c.send(&hello("nobody", None)).await;
    assert!(matches!(c.hub().await, BrokerToHub::Error(_)));
    let refused = with_message(&h, "connection refused");
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].level, Level::Warn);
    assert_eq!(field(&refused[0], "code"), Some("unauthorized"));
}

#[tokio::test]
async fn a_rule_that_holds_an_order_is_named_in_the_log_once() {
    let h = Harness::start(
        "[[rule]]\nname = \"ci-reserve\"\nwhen = { client_label = \"ci\" }\n\
         reserve = { cpu = 6, mem_mb = 1024 }\n",
    );
    let mut r1 = h.member_runner("r1", "10.0.0.7", linux()).await;
    let _r2 = h
        .member_runner("r2", "10.0.0.8", caps(&["linux"], 8000, 8))
        .await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&order("j1", req(512, 1, 60))).await;
    alex.accepted().await;
    r1.assign().await;
    alex.send(&order("j2", req(512, 2, 60))).await;
    alex.accepted().await;
    alex.send(&order("j3", req(256, 1, 60))).await;
    alex.accepted().await;
    h.poke().await;
    alex.sync().await;

    let held = one(&h, "order held by a policy rule", "j2");
    assert_eq!(field(&held, "rule"), Some("ci-reserve"));
    assert!(with_message(&h, "order held by a policy rule")
        .iter()
        .all(|r| field(r, "job") != Some("j1")));
}

#[tokio::test]
async fn reload_logs_policy_reloaded_and_a_refused_reload_does_not() {
    let h = Harness::start("");
    h.handle
        .reload_policy(&policy("[grace]\nlost_min = 20\n"))
        .await
        .expect("reload applies");
    let reloaded = with_message(&h, "policy reloaded");
    assert_eq!(reloaded.len(), 1);
    assert_eq!(reloaded[0].level, Level::Info);
    assert_eq!(reloaded[0].role, Role::Broker);
    assert!(h
        .handle
        .reload_policy(&policy("[grace]\nlost_min = 0\n"))
        .await
        .is_err());
    assert_eq!(with_message(&h, "policy reloaded").len(), 1);
}

#[tokio::test]
async fn job_output_and_commands_never_reach_a_record() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    let cmd = "echo token=cmd-4f1c9e";
    hub.send(&with_command(order("j1", req(512, 1, 60)), cmd))
        .await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    r.send(&chunk("j1", 2, "out-7d2a1b password=hunter2\n"))
        .await;
    r.send(&result("j1", 3, JobState::Failed, 1)).await;
    r.acked(3).await;
    h.until_state("local:alex", "j1", JobState::Failed).await;
    hub.send(&with_command(order("j2", req(99_999, 1, 60)), cmd))
        .await;
    hub.rejected().await;

    let records = h.log.records();
    assert!(
        records.iter().any(|r| field(r, "job") == Some("j1")),
        "the run is logged at all"
    );
    for r in &records {
        let text = format!(
            "{} {}",
            r.message,
            r.fields
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        for secret in ["cmd-4f1c9e", "echo token", "out-7d2a1b", "hunter2"] {
            assert!(!text.contains(secret), "{secret} leaked into {text}");
        }
    }
}
