mod common;

use common::harness::*;
use common::{entry, HOST};
use lr_broker::policy::{load, ActivePolicy};
use lr_core::{JobId, JobState, ServeLog, Timestamp};
use lr_proto::BrokerToRunner;
use std::time::Duration;

const IN_CI_NIGHT: &str =
    "[[rule]]\nwhen = { client_label = { in = [\"ci\", \"night\"] } }\npriority = 50\n";

#[tokio::test]
async fn condition_in_gives_priority_and_goes_next() {
    let h = Harness::start(IN_CI_NIGHT);
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    let mut night = h.member_hub("night-box", "night", "10.0.0.21").await;
    alex.send(&order_at("j0", req(512, 1, 60), START)).await;
    alex.accepted().await;
    r.assign().await;
    alex.send(&order_at("a1", req(512, 1, 60), START + 1)).await;
    alex.accepted().await;
    night
        .send(&order_at("n1", req(512, 1, 60), START + 2))
        .await;
    night.accepted().await;
    r.send(&result("j0", 1, JobState::Passed, 0)).await;
    assert_eq!(r.assign().await.job, JobId("n1".into()));
}

#[tokio::test]
async fn higher_priority_goes_next_without_stopping_the_running_job() {
    let h = Harness::start(
        "[[rule]]\nwhen = { client_label = { prefix = \"ci\" }, mem_mb = { ge = 256 } }\npriority = 50\n",
    );
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    let mut ci = h.member_hub("ci-box", "ci", "10.0.0.20").await;
    alex.send(&order("a1", req(512, 1, 60))).await;
    alex.accepted().await;
    r.assign().await;
    r.send(&started("a1", 1)).await;
    ci.send(&order("c1", req(512, 1, 60))).await;
    assert!(ci.accepted().await.waiting.is_some());
    alex.sync().await;
    match r.runner().await {
        BrokerToRunner::Ack(a) => assert_eq!(a.seq, 1),
        other => panic!("unexpected {other:?}"),
    }
    r.quiet().await;
    assert_eq!(
        h.job_row("local:alex", "a1").await.unwrap().job.state,
        JobState::Running
    );
}

async fn first_after_running(first: (&str, &str), second: (&str, &str)) -> String {
    let h = Harness::start(IN_CI_NIGHT);
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    let mut one = h
        .member_hub(&format!("{}-box", first.1), first.1, "10.0.0.20")
        .await;
    let mut two = h
        .member_hub(&format!("{}-box", second.1), second.1, "10.0.0.21")
        .await;
    alex.send(&order("j0", req(512, 1, 60))).await;
    alex.accepted().await;
    r.assign().await;
    one.send(&order(first.0, req(512, 1, 60))).await;
    one.accepted().await;
    h.clock.advance(Duration::from_secs(1));
    two.send(&order(second.0, req(512, 1, 60))).await;
    two.accepted().await;
    r.send(&result("j0", 1, JobState::Passed, 0)).await;
    r.assign().await.job.0
}

#[tokio::test]
async fn equal_priority_takes_the_oldest() {
    assert_eq!(
        first_after_running(("z-night", "night"), ("a-ci", "ci")).await,
        "z-night"
    );
    assert_eq!(
        first_after_running(("a-ci", "ci"), ("z-night", "night")).await,
        "a-ci"
    );
}

#[test]
fn a_profile_label_raises_priority_only_for_orders_that_carry_it() {
    let f =
        load("[[rule]]\nwhen = { client_label = \"agent\", label = \"work\" }\npriority = 50\n")
            .unwrap();
    let policy = lr_broker::policy::rule_policy(&f, &HOST);
    let mut labelled = entry("w", "agent", 0);
    labelled.order.declared.labels = vec!["x".into(), "work".into()];
    let plain = entry("p", "agent", 0);
    let mut other_client = entry("o", "someone", 0);
    other_client.order.declared.labels = vec!["work".into()];
    assert_eq!(policy.priority_of(&labelled), 50);
    assert_eq!(policy.priority_of(&plain), 0);
    assert_eq!(policy.priority_of(&other_client), 0);
}

#[test]
fn label_forms_in_glob_and_not_match_any_label() {
    let f = load(
        "[[rule]]\nwhen = { client_label = \"agent\", label = { glob = \"rel*\" }, not = { label = \"hold\" } }\npriority = 7\n",
    )
    .unwrap();
    let policy = lr_broker::policy::rule_policy(&f, &HOST);
    let with = |labels: &[&str]| {
        let mut e = entry("j", "agent", 0);
        e.order.declared.labels = labels.iter().map(|s| s.to_string()).collect();
        policy.priority_of(&e)
    };
    assert_eq!(with(&["a", "release"]), 7);
    assert_eq!(with(&["release", "hold"]), 0);
    assert_eq!(with(&["a"]), 0);
}

#[test]
fn claimed_attribute_without_trusted_condition_is_rejected_on_load() {
    let e = load("[[rule]]\nwhen = { project = { glob = \"lr-*\" } }\npriority = 9\n").unwrap_err();
    assert!(e.message.contains("client_label"), "{e}");
    let e = load("[[rule]]\nwhen = { label = \"work\" }\npriority = 9\n").unwrap_err();
    assert!(
        e.message.contains("client_label") && e.message.contains("label"),
        "{e}"
    );
    load(
        "[[rule]]\nwhen = { client_label = { in = [\"ci\"] }, project = { glob = \"lr-*\" } }\npriority = 9\n",
    )
    .expect("a trusted condition guards the claimed one");
}

#[test]
fn reload_applies_new_conditions_and_keeps_old_on_error() {
    let queue = [entry("old", "agent", 0), entry("ci", "ci", 10)];
    let order = |a: &ActivePolicy| {
        a.policy()
            .order(&queue, &ServeLog::default(), Timestamp(20))
    };
    let (mut active, _) = ActivePolicy::start(
        "[[rule]]\nwhen = { client_label = \"ci\" }\npriority = 100\n",
        HOST,
        false,
    )
    .unwrap();
    assert_eq!(order(&active), vec![1, 0]);
    active
        .reload(
            "[[rule]]\nwhen = { not = { client_label = \"ci\" }, client_label = \"agent\" }\npriority = 100\n",
        )
        .unwrap();
    assert_eq!(order(&active), vec![0, 1]);
    assert!(active
        .reload("[[rule]]\nwhen = { any = [] }\npriority = 100\n")
        .is_err());
    assert_eq!(order(&active), vec![0, 1]);
    assert_eq!(active.file().rules.len(), 1);
}

#[tokio::test]
async fn negative_priority_orders_last() {
    let h = Harness::start("[[rule]]\nwhen = { client_label = \"bulk\" }\npriority = -10\n");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    let mut bulk = h.member_hub("bulk-box", "bulk", "10.0.0.20").await;
    alex.send(&order("j0", req(512, 1, 60))).await;
    alex.accepted().await;
    r.assign().await;
    bulk.send(&order("a-bulk", req(512, 1, 60))).await;
    bulk.accepted().await;
    h.clock.advance(Duration::from_secs(1));
    alex.send(&order("z-alex", req(512, 1, 60))).await;
    alex.accepted().await;
    r.send(&result("j0", 1, JobState::Passed, 0)).await;
    assert_eq!(r.assign().await.job, JobId("z-alex".into()));
}
