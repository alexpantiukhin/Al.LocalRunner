mod common;

use common::broker::{OnWithdraw, Script};
use common::*;
use lr_core::Zone;
use lr_proto::{CancelRequest, CancelTarget, CliReply, CliRequest, ReasonCode};

async fn wait_connected(brokers: &[&common::broker::FakeBroker]) {
    for b in brokers {
        until(b, "broker connected", || b.resumed()).await;
    }
    settle().await;
}

#[tokio::test]
async fn a_waiting_order_is_asked_back_and_placed_where_it_starts_now() {
    let mut rig = Rig::new();
    rig.join("far").await;
    rig.join("lan-a").await;
    let far = rig.broker(
        "far",
        Zone::Remote,
        Script::linux(Zone::Remote).start_now(false),
    );
    let lan = rig.broker(
        "lan-a",
        Zone::Lan,
        Script::linux(Zone::Lan)
            .start_now(false)
            .on_withdraw(OnWithdraw::Return),
    );
    rig.start();
    wait_connected(&[&far, &lan]).await;
    rig.cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    until(&lan, "order placed at the nearest broker", || {
        lan.orders().len() == 1
    })
    .await;
    settle().await;
    far.set(|s| s.start_now = Some(true));
    rig.step(16).await;
    until(&lan, "withdrawn", || lan.withdraws() == 1).await;
    until(&far, "placed again", || far.orders().len() == 1).await;
}

#[tokio::test]
async fn nothing_is_asked_back_while_no_other_broker_starts_the_order_now() {
    let mut rig = Rig::new();
    rig.join("far").await;
    rig.join("lan-a").await;
    let far = rig.broker(
        "far",
        Zone::Remote,
        Script::linux(Zone::Remote).start_now(false),
    );
    let lan = rig.broker(
        "lan-a",
        Zone::Lan,
        Script::linux(Zone::Lan).start_now(false),
    );
    rig.start();
    wait_connected(&[&far, &lan]).await;
    rig.cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    until(&lan, "order placed", || lan.orders().len() == 1).await;
    rig.step(40).await;
    assert_eq!(lan.withdraws(), 0);
    assert!(far.orders().is_empty());
}

#[tokio::test]
async fn a_started_order_is_never_asked_back() {
    let mut rig = Rig::new();
    rig.join("far").await;
    rig.join("lan-a").await;
    let far = rig.broker(
        "far",
        Zone::Remote,
        Script::linux(Zone::Remote).start_now(false),
    );
    let lan = rig.broker(
        "lan-a",
        Zone::Lan,
        Script::linux(Zone::Lan).start_now(false),
    );
    rig.start();
    wait_connected(&[&far, &lan]).await;
    rig.cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    until(&lan, "order placed", || lan.orders().len() == 1).await;
    let job = lan.orders()[0].job.clone();
    lan.started(&job).await;
    settle().await;
    far.set(|s| s.start_now = Some(true));
    rig.step(40).await;
    assert_eq!(lan.withdraws(), 0);
}

#[tokio::test]
async fn a_broker_that_does_not_say_start_now_is_not_asked_to_give_an_order_back() {
    let mut rig = Rig::new();
    rig.join("far").await;
    rig.join("lan-a").await;
    let far = rig.broker(
        "far",
        Zone::Remote,
        Script::linux(Zone::Remote).start_now(true),
    );
    let lan = rig.broker("lan-a", Zone::Lan, Script::linux(Zone::Lan));
    rig.start();
    wait_connected(&[&far, &lan]).await;
    rig.cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    until(&lan, "order placed", || lan.orders().len() == 1).await;
    rig.step(40).await;
    assert_eq!(lan.withdraws(), 0);
}

#[tokio::test]
async fn an_order_that_stays_is_asked_again_only_after_the_pause() {
    let mut rig = Rig::new();
    rig.join("far").await;
    rig.join("lan-a").await;
    let far = rig.broker(
        "far",
        Zone::Remote,
        Script::linux(Zone::Remote).start_now(false),
    );
    let lan = rig.broker(
        "lan-a",
        Zone::Lan,
        Script::linux(Zone::Lan).start_now(false),
    );
    rig.start();
    wait_connected(&[&far, &lan]).await;
    rig.cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    until(&lan, "order placed", || lan.orders().len() == 1).await;
    settle().await;
    far.set(|s| s.start_now = Some(true));
    rig.step(16).await;
    until(&lan, "asked back", || lan.withdraws() == 1).await;
    rig.step(40).await;
    assert_eq!(lan.withdraws(), 1, "the order is left alone for 60 s");
    rig.step(30).await;
    until(&lan, "asked again", || lan.withdraws() == 2).await;
}

#[tokio::test]
async fn an_order_limited_to_the_local_zone_is_never_asked_back() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner().start_now(false));
    rig.join("far").await;
    let far = rig.broker(
        "far",
        Zone::Remote,
        Script::linux(Zone::Remote).start_now(true),
    );
    rig.start();
    wait_connected(&[&local, &far]).await;
    rig.cli().await.submit(submit("local-only", None)).await;
    until(&local, "order placed", || local.orders().len() == 1).await;
    rig.step(40).await;
    assert_eq!(local.withdraws(), 0);
}

#[tokio::test]
async fn a_cancel_while_the_order_is_asked_back_wins_over_the_move() {
    let mut rig = Rig::new();
    rig.join("far").await;
    rig.join("lan-a").await;
    let far = rig.broker(
        "far",
        Zone::Remote,
        Script::linux(Zone::Remote).start_now(false),
    );
    let lan = rig.broker(
        "lan-a",
        Zone::Lan,
        Script::linux(Zone::Lan).start_now(false),
    );
    rig.start();
    wait_connected(&[&far, &lan]).await;
    let cli = rig.cli().await;
    let job = cli.submit(submit("remote", Some("linux"))).await;
    until(&lan, "order placed", || lan.orders().len() == 1).await;
    settle().await;
    far.set(|s| s.start_now = Some(true));
    rig.step(16).await;
    until(&lan, "asked back", || lan.withdraws() == 1).await;
    cli.send(CliRequest::Cancel(CancelRequest {
        actor: identity("agent"),
        target: CancelTarget::Job(job.clone()),
        force: false,
        reason: None,
    }))
    .await;
    assert!(matches!(cli.reply().await, CliReply::Cancelled { .. }));
    lan.returned(&job, ReasonCode::Moved).await;
    settle().await;
    assert!(
        far.orders().is_empty(),
        "a cancelled order is not placed again"
    );
    assert!(rig.active(&job).await.is_none());
}
