mod common;

use common::harness::*;
use lr_core::{StepSpec, Zone};
use lr_proto::*;
use std::collections::BTreeMap;

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
    assert_eq!(assign.spec.code, WireCode::Own);
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
