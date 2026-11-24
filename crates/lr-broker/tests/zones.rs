mod common;

use common::harness::*;
use lr_abstractions::MachineKind;
use lr_core::{JobState, StepSpec, Zone};
use lr_proto::*;
use std::collections::BTreeMap;

const NETWORK: &str = "[networks]\nhome = [\"10.0.0.0/24\"]\n";
const POLICY: &str =
    "[networks]\nhome = [\"10.0.0.0/24\"]\n\n[[shared]]\nmembers = [\"dev\", \"docker-1\"]\n";

fn step(command: &str) -> StepSpec {
    StepSpec {
        name: "run".into(),
        command: command.into(),
        timeout_s: 60,
        mem_mb: None,
        cpu: None,
        tags: None,
        secrets: Vec::new(),
        continue_on_failure: false,
    }
}

fn zoned_order(job: &str, zones: &[Zone]) -> HubToBroker {
    let mut requirements = req(1024, 1, 60);
    requirements.zones = Some(zones.iter().copied().collect());
    let mut msg = order(job, requirements);
    if let HubToBroker::Order(o) = &mut msg {
        o.spec.steps = vec![step("common")];
        o.spec.zone_steps = BTreeMap::from([
            (Zone::Shared, vec![step("shared")]),
            (Zone::Lan, vec![step("lan")]),
        ]);
    }
    msg
}

#[tokio::test]
async fn a_runner_named_with_the_client_in_one_shared_block_gets_the_shared_steps() {
    let h = Harness::start(POLICY);
    let mut runner = h.member_runner("docker-1", "10.0.0.9", linux()).await;
    let mut hub = h.member_hub("hub", "dev", "10.0.0.5").await;
    hub.send(&zoned_order("a", &[Zone::Shared, Zone::Lan]))
        .await;
    hub.accepted().await;
    let assign = runner.assign().await;
    assert_eq!(assign.spec.steps, vec![step("shared")]);
    assert!(matches!(assign.spec.code, WireCode::WorkTree { .. }));
}

#[tokio::test]
async fn a_runner_outside_every_block_but_in_the_network_gets_the_lan_steps() {
    let h = Harness::start(POLICY);
    let mut runner = h.member_runner("other", "10.0.0.8", linux()).await;
    let mut hub = h.member_hub("hub", "dev", "10.0.0.5").await;
    hub.send(&zoned_order("a", &[Zone::Shared, Zone::Lan]))
        .await;
    hub.accepted().await;
    let assign = runner.assign().await;
    assert_eq!(assign.spec.steps, vec![step("lan")]);
    assert_eq!(assign.spec.code, WireCode::Own);
}

#[tokio::test]
async fn an_order_that_allows_only_the_shared_zone_does_not_go_to_a_lan_runner() {
    let h = Harness::start(POLICY);
    let mut runner = h.member_runner("other", "10.0.0.8", linux()).await;
    let mut hub = h.member_hub("hub", "dev", "10.0.0.5").await;
    hub.send(&zoned_order("a", &[Zone::Shared])).await;
    let refused = hub.rejected().await;
    assert!(
        refused.detail.contains("zone"),
        "the refusal names the zone: {}",
        refused.detail
    );
    runner.quiet().await;
}

#[tokio::test]
async fn a_client_label_outside_the_block_does_not_make_the_zone_shared() {
    let h = Harness::start(POLICY);
    let mut runner = h.member_runner("docker-1", "10.0.0.9", linux()).await;
    let mut hub = h.member_hub("hub", "stranger", "10.0.0.5").await;
    hub.send(&zoned_order("a", &[Zone::Shared, Zone::Lan]))
        .await;
    hub.accepted().await;
    let assign = runner.assign().await;
    assert_eq!(assign.spec.steps, vec![step("lan")]);
}

#[tokio::test]
async fn an_order_that_allows_a_zone_without_steps_is_refused_at_intake() {
    let h = Harness::start(POLICY);
    let mut runner = h.member_runner("other", "10.0.0.8", linux()).await;
    let mut hub = h.member_hub("hub", "dev", "10.0.0.5").await;
    hub.send(&zoned_order("a", &[Zone::Remote])).await;
    let refused = hub.rejected().await;
    assert!(
        refused.detail.contains("remote"),
        "the refusal names the zone: {}",
        refused.detail
    );
    runner.quiet().await;
}

async fn hub_with_zone(h: &Harness, name: &str, label: &str, addr: &str) -> (Conn, Option<Zone>) {
    let key = h.seed(name, MachineKind::Hub, label).await;
    let mut c = h.dial(member_peer(&key, addr)).await;
    c.send(&hello(name, None)).await;
    let zone = c.welcome().await.zone;
    (c, zone)
}

#[tokio::test]
async fn reload_applies_networks_to_new_connections() {
    let h = Harness::start("");
    let mut runner = h.member_runner("other", "10.0.0.8", linux()).await;
    let (mut before, zone) = hub_with_zone(&h, "hub-a", "dev", "10.0.0.5").await;
    assert_eq!(zone, Some(Zone::Remote));
    let applied = h
        .handle
        .reload_policy(&policy(NETWORK))
        .await
        .expect("reload applies");
    assert!(applied.restart_required.is_empty(), "{applied:?}");
    let (mut after, zone) = hub_with_zone(&h, "hub-b", "ops", "10.0.0.6").await;
    assert_eq!(
        zone,
        Some(Zone::Lan),
        "a new connection gets the new network"
    );
    after.send(&zoned_order("a", &[Zone::Lan])).await;
    after.accepted().await;
    assert_eq!(runner.assign().await.spec.steps, vec![step("lan")]);
    let msgs = before.sync().await;
    assert!(
        !msgs.iter().any(|m| matches!(m, BrokerToHub::Welcome(_))),
        "an established connection keeps the zone of its hello: {msgs:?}"
    );
}

#[tokio::test]
async fn reload_applies_shared_to_new_orders() {
    let h = Harness::start(NETWORK);
    let mut runner = h.member_runner("docker-1", "10.0.0.9", linux()).await;
    let mut hub = h.member_hub("hub", "dev", "10.0.0.5").await;
    hub.send(&zoned_order("before", &[Zone::Shared, Zone::Lan]))
        .await;
    hub.accepted().await;
    assert_eq!(runner.assign().await.spec.steps, vec![step("lan")]);
    runner.send(&started("before", 1)).await;
    hub.started_notice().await;
    let applied = h
        .handle
        .reload_policy(&policy(POLICY))
        .await
        .expect("reload applies");
    assert!(applied.restart_required.is_empty(), "{applied:?}");
    hub.send(&zoned_order("after", &[Zone::Shared, Zone::Lan]))
        .await;
    hub.accepted().await;
    runner.send(&result("before", 2, JobState::Passed, 0)).await;
    let done = h.until_state("hub", "before", JobState::Passed).await;
    assert_eq!(done.job.cancel_reason, None, "the running job is untouched");
    let a = runner.assign().await;
    assert_eq!(
        (a.job.0.as_str(), a.spec.steps),
        ("after", vec![step("shared")]),
        "the order after the reload sees the new shared set"
    );
}
