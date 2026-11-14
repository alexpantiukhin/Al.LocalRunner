mod common;

use common::broker::Script;
use common::*;
use lr_abstractions::Clock;
use lr_core::Timestamp;
use lr_proto::{BrokerStatus, CliReply, CliRequest};

async fn local_status(rig: &Rig) -> BrokerStatus {
    let cli = rig.cli().await;
    cli.send(CliRequest::Brokers).await;
    let reply = cli.reply().await;
    cli.close().await;
    match reply {
        CliReply::Brokers { mut brokers } => brokers.remove(0),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn since_is_the_hub_start_while_the_broker_never_answered() {
    let mut rig = Rig::new();
    rig.local = true;
    rig.net.set_reachable(LOCAL_IPC, false);
    rig.start();
    settle().await;
    rig.step(40).await;
    let b = local_status(&rig).await;
    assert!(!b.available, "{b:?}");
    assert_eq!(b.since, Some(Timestamp(START)), "{b:?}");
}

#[tokio::test]
async fn since_moves_to_the_loss_when_a_connected_broker_goes_away() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    eventually("connected", || local.hellos() == 1).await;
    settle().await;
    rig.step(5).await;
    rig.net.set_reachable(LOCAL_IPC, false);
    let lost = rig.clock.now();
    local.disconnect().await;
    settle().await;
    let b = local_status(&rig).await;
    assert!(!b.available, "{b:?}");
    assert_eq!(b.since, Some(lost), "{b:?}");
    assert_ne!(b.since, Some(Timestamp(START)));
}

#[tokio::test]
async fn since_stays_at_the_first_loss_across_failed_redials() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    eventually("connected", || local.hellos() == 1).await;
    settle().await;
    rig.step(5).await;
    rig.net.set_reachable(LOCAL_IPC, false);
    let lost = rig.clock.now();
    local.disconnect().await;
    settle().await;
    rig.step(90).await;
    let b = local_status(&rig).await;
    assert!(!b.available, "{b:?}");
    assert_eq!(b.since, Some(lost), "{b:?}");
}

#[tokio::test]
async fn since_moves_to_the_return_when_the_broker_comes_back() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    eventually("connected", || local.hellos() == 1).await;
    settle().await;
    rig.step(5).await;
    rig.net.set_reachable(LOCAL_IPC, false);
    let lost = rig.clock.now();
    local.disconnect().await;
    settle().await;
    rig.step(10).await;
    rig.net.set_reachable(LOCAL_IPC, true);
    rig.step(40).await;
    eventually("reconnected", || local.hellos() == 2).await;
    settle().await;
    let b = local_status(&rig).await;
    assert!(b.available, "{b:?}");
    let since = b.since.expect("since is set");
    assert!(
        since > lost && since <= rig.clock.now(),
        "{b:?} lost {lost:?}"
    );
}
