mod common;

use common::broker::Script;
use common::*;
use lr_core::Zone;
use lr_proto::{CliReply, ReasonCode, RejectReason};

fn limit_profiles_to_lan(rig: &Rig) {
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
}

async fn connected(brokers: &[&common::broker::FakeBroker]) {
    for b in brokers {
        eventually("broker connected", || b.resumed()).await;
    }
    settle().await;
}

#[tokio::test]
async fn same_machine_order_waits_while_the_local_broker_is_down() {
    let mut rig = Rig::new();
    rig.join("lan-a").await;
    rig.net.set_reachable(LOCAL_IPC, false);
    let local = rig.local_broker(Script::local_runner());
    rig.net.set_reachable(LOCAL_IPC, false);
    let lan = rig.broker("lan-a", Zone::Lan, Script::linux(Zone::Lan));
    rig.start();
    connected(&[&lan]).await;
    let job = rig.cli().await.submit(submit("unit", None)).await;
    rig.step(60).await;
    assert!(
        rig.active(&job).await.is_some(),
        "the order waits instead of being refused"
    );
    assert_eq!(rig.pending().await.len(), 1, "it stays in the outbox");
    assert_eq!(lan.queries(), 0, "a network broker is never asked");
    assert!(lan.orders().is_empty(), "it never moves to another machine");
    assert!(
        local.orders().is_empty(),
        "the local broker was really down"
    );
    rig.net.set_reachable(LOCAL_IPC, true);
    rig.step(40).await;
    eventually("placed once the local broker is back", || {
        local.orders().len() == 1
    })
    .await;
    assert!(lan.orders().is_empty());
    assert_eq!(local.orders()[0].spec.requirements.platform, None);
}

#[tokio::test]
async fn zone_admitting_no_broker_is_rejected_not_kept_in_outbox() {
    let mut rig = Rig::new();
    limit_profiles_to_lan(&rig);
    rig.join("far").await;
    let far = rig.broker("far", Zone::Remote, Script::linux(Zone::Remote));
    rig.start();
    connected(&[&far]).await;
    let cli = rig.cli().await;
    let mut req = submit("remote", Some("linux"));
    req.wait = true;
    let job = cli.submit(req).await;
    match cli.reply().await {
        CliReply::Rejected { code, detail } => {
            assert_eq!(code, ReasonCode::Rejected(RejectReason::ZoneNotServed));
            assert_eq!(detail, "нет брокеров в допустимых зонах заказа");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        rig.pending().await.is_empty(),
        "nothing waits in the outbox"
    );
    assert!(rig.active(&job).await.is_none());
    rig.step(60).await;
    assert_eq!(far.queries(), 0);
    assert!(
        far.orders().is_empty(),
        "no later retry crosses the boundary"
    );
}

#[tokio::test]
async fn returned_order_keeps_its_zone_limit_and_skips_the_returning_broker() {
    let mut rig = Rig::new();
    limit_profiles_to_lan(&rig);
    rig.policy.brokers.order = vec!["lan-a".into(), "lan-b".into()];
    for name in ["lan-a", "lan-b", "far"] {
        rig.join(name).await;
    }
    let a = rig.broker("lan-a", Zone::Lan, Script::linux(Zone::Lan));
    let b = rig.broker("lan-b", Zone::Lan, Script::linux(Zone::Lan));
    let far = rig.broker("far", Zone::Remote, Script::linux(Zone::Remote));
    rig.start();
    connected(&[&a, &b, &far]).await;
    let job = rig
        .cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    eventually("accepted by lan-a", || a.orders().len() == 1).await;
    settle().await;
    let queries_of_a = a.queries();
    a.returned(&job, ReasonCode::RunnerRemoved).await;
    eventually("replacement at lan-b", || b.orders().len() == 1).await;
    settle().await;
    assert_eq!(
        a.orders().len(),
        1,
        "the returning broker gets no second copy"
    );
    assert_eq!(a.queries(), queries_of_a, "nor is it asked again");
    assert_eq!(far.queries(), 0, "the zone boundary still holds");
    assert!(far.orders().is_empty());
    assert_eq!(
        rig.active(&job).await.unwrap().broker.as_deref(),
        Some("lan-b")
    );
}

#[tokio::test]
async fn returned_order_with_no_broker_left_in_its_zone_is_rejected_not_sent_farther() {
    let mut rig = Rig::new();
    limit_profiles_to_lan(&rig);
    rig.join("lan-a").await;
    rig.join("far").await;
    let a = rig.broker("lan-a", Zone::Lan, Script::linux(Zone::Lan));
    let far = rig.broker("far", Zone::Remote, Script::linux(Zone::Remote));
    rig.start();
    connected(&[&a, &far]).await;
    let cli = rig.cli().await;
    let mut req = submit("remote", Some("linux"));
    req.wait = true;
    let job = cli.submit(req).await;
    eventually("accepted by lan-a", || a.orders().len() == 1).await;
    settle().await;
    a.returned(&job, ReasonCode::RunnerRemoved).await;
    match cli.reply().await {
        CliReply::Rejected { code, detail } => {
            assert_eq!(code, ReasonCode::RunnerRemoved);
            assert_eq!(detail, "runner gone");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(far.queries(), 0);
    assert!(far.orders().is_empty());
    assert_eq!(a.orders().len(), 1);
    assert!(rig.active(&job).await.is_none());
}
