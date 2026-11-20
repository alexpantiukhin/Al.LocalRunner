mod common;

use common::broker::Script;
use common::*;
use lr_core::Zone;
use lr_proto::{CliReply, CliRequest, ProtocolErrorCode, WireCode};

fn zones(list: &[Zone]) -> Option<std::collections::BTreeSet<Zone>> {
    Some(list.iter().copied().collect())
}

#[tokio::test]
async fn a_profile_with_zone_variants_is_sent_with_the_steps_of_every_zone() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    rig.cli().await.submit(submit("zoned", None)).await;
    until(&local, "order sent", || local.orders().len() == 1).await;
    let spec = &local.orders()[0].spec;
    assert_eq!(spec.requirements.same_machine, None);
    assert_eq!(
        spec.requirements.zones,
        zones(&[Zone::Local, Zone::Shared, Zone::Lan])
    );
    assert!(matches!(spec.code, WireCode::WorkTree { .. }));
    assert_eq!(spec.steps[0].command, "cargo test all");
    let commands: Vec<(Zone, &str)> = spec
        .zone_steps
        .iter()
        .map(|(z, s)| (*z, s[0].command.as_str()))
        .collect();
    assert_eq!(
        commands,
        [
            (Zone::Local, "cargo test all"),
            (Zone::Shared, "docker exec b cargo test all"),
            (Zone::Lan, "git fetch origin all && cargo test all"),
        ]
    );
}

#[tokio::test]
async fn zones_of_the_order_narrow_the_zones_of_the_profile() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let mut req = submit("zoned", None);
    req.zones = zones(&[Zone::Local, Zone::Shared]);
    rig.cli().await.submit(req).await;
    until(&local, "order sent", || local.orders().len() == 1).await;
    let spec = &local.orders()[0].spec;
    assert_eq!(spec.requirements.zones, zones(&[Zone::Local, Zone::Shared]));
    assert_eq!(
        spec.zone_steps.keys().copied().collect::<Vec<_>>(),
        [Zone::Local, Zone::Shared]
    );
}

#[tokio::test]
async fn zones_the_profile_has_no_steps_for_are_refused() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    let mut req = submit("zoned", None);
    req.zones = zones(&[Zone::Remote]);
    cli.send(CliRequest::Submit(req)).await;
    match cli.reply().await {
        CliReply::Error(e) => {
            assert_eq!(e.code, ProtocolErrorCode::Malformed);
            assert!(e.message.contains("names no zone"), "{}", e.message);
        }
        other => panic!("expected an error, got {other:?}"),
    }
    assert!(local.orders().is_empty());
}

#[tokio::test]
async fn a_profile_without_variants_runs_in_local_and_shared_when_zones_are_asked() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let mut req = submit("unit", None);
    req.zones = zones(&[Zone::Local, Zone::Shared, Zone::Lan]);
    rig.cli().await.submit(req).await;
    until(&local, "order sent", || local.orders().len() == 1).await;
    let spec = &local.orders()[0].spec;
    assert_eq!(spec.requirements.zones, zones(&[Zone::Local, Zone::Shared]));
    assert_eq!(spec.requirements.same_machine, None);
}
