mod common;

use common::broker::Script;
use common::*;
use lr_core::Zone;
use lr_proto::{CliReply, ReasonCode, RejectReason};

#[tokio::test]
async fn a_broker_reconnecting_from_another_network_is_judged_by_its_new_zone() {
    let mut rig = Rig::new();
    let profiles = PROFILES.replace(
        "platforms = [\"linux\"]",
        "platforms = [\"linux\"]\nzone = \"lan\"",
    );
    rig.repo
        .commit_file(COMMIT, ".localrunner/profiles.toml", profiles.as_bytes());
    rig.files.put(
        &repo_root().join(".localrunner").join("profiles.toml"),
        profiles.as_bytes(),
    );
    rig.join("laptop").await;
    let laptop = rig.broker("laptop", Zone::Lan, Script::linux(Zone::Lan));
    rig.start();
    eventually("connected", || laptop.resumed()).await;
    settle().await;

    let cli = rig.cli().await;
    cli.submit(submit("remote", Some("linux"))).await;
    eventually("the lan order is placed", || laptop.orders().len() == 1).await;

    laptop.report_zone(Some(Zone::Remote));
    laptop.disconnect().await;
    for _ in 0..300 {
        if laptop.hellos() == 2 {
            break;
        }
        rig.step(1).await;
    }
    assert_eq!(laptop.hellos(), 2, "the hub connects again");
    settle().await;

    let mut req = submit("remote", Some("linux"));
    req.wait = true;
    let again = rig.cli().await;
    again.submit(req).await;
    match again.reply().await {
        CliReply::Rejected { code, .. } => {
            assert_eq!(code, ReasonCode::Rejected(RejectReason::ZoneNotServed))
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(laptop.orders().len(), 1, "no lan order goes to a remote broker");
}
