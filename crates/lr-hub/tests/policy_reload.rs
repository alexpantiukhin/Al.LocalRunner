mod common;

use common::broker::Script;
use common::*;
use lr_core::Zone;
use lr_hub::{parse_hub_policy, Hub, ReloadPolicy};
use lr_proto::{CliReply, CliRequest, ProtocolErrorCode};
use std::sync::{Arc, Mutex};

fn reloader(file: &Arc<Mutex<String>>) -> ReloadPolicy {
    let file = file.clone();
    Arc::new(move || {
        let text = file.lock().unwrap().clone();
        parse_hub_policy(&text).map_err(|e| e.to_string())
    })
}

#[tokio::test]
async fn a_changed_brokers_order_applies_to_the_next_order() {
    let mut rig = Rig::new();
    rig.policy.brokers.order = vec!["lan-a".into(), "lan-b".into()];
    for name in ["lan-a", "lan-b"] {
        rig.join(name).await;
    }
    let a = rig.broker("lan-a", Zone::Lan, Script::linux(Zone::Lan));
    let b = rig.broker("lan-b", Zone::Lan, Script::linux(Zone::Lan));
    let file = Arc::new(Mutex::new(
        "[brokers]\norder = [\"lan-b\", \"lan-a\"]\n".to_string(),
    ));
    let hub = Hub::new(rig.deps()).with_policy_reload(reloader(&file));
    tokio::spawn(hub.run());
    for x in [&a, &b] {
        until(x, "broker connected", || x.resumed()).await;
    }
    settle().await;
    let cli = rig.cli().await;
    cli.submit(submit("remote", Some("linux"))).await;
    until(&a, "first order placed", || a.orders().len() == 1).await;
    cli.send(CliRequest::PolicyReload).await;
    assert_eq!(cli.reply().await, CliReply::Reloaded);
    cli.submit(submit("remote", Some("linux"))).await;
    until(&b, "second order placed", || b.orders().len() == 1).await;
    assert_eq!(a.orders().len(), 1, "the order before the reload stays");
}

#[tokio::test]
async fn an_invalid_hub_policy_keeps_the_old_one() {
    let mut rig = Rig::new();
    rig.policy.brokers.order = vec!["lan-a".into(), "lan-b".into()];
    for name in ["lan-a", "lan-b"] {
        rig.join(name).await;
    }
    let a = rig.broker("lan-a", Zone::Lan, Script::linux(Zone::Lan));
    let b = rig.broker("lan-b", Zone::Lan, Script::linux(Zone::Lan));
    let file = Arc::new(Mutex::new("[brokers]\nstrategy = \"nope\"\n".to_string()));
    let hub = Hub::new(rig.deps()).with_policy_reload(reloader(&file));
    tokio::spawn(hub.run());
    for x in [&a, &b] {
        until(x, "broker connected", || x.resumed()).await;
    }
    settle().await;
    let cli = rig.cli().await;
    cli.send(CliRequest::PolicyReload).await;
    match cli.reply().await {
        CliReply::Error(e) => {
            assert_eq!(e.code, ProtocolErrorCode::Malformed);
            assert!(e.message.contains("strategy"), "{}", e.message);
        }
        other => panic!("{other:?}"),
    }
    cli.submit(submit("remote", Some("linux"))).await;
    until(&a, "order placed", || a.orders().len() == 1).await;
    assert!(b.orders().is_empty(), "the old order [lan-a, lan-b] stays");
}

#[tokio::test]
async fn a_hub_without_a_reloader_refuses_reload() {
    let mut rig = Rig::new();
    tokio::spawn(Hub::new(rig.deps()).run());
    settle().await;
    let cli = rig.cli().await;
    cli.send(CliRequest::PolicyReload).await;
    assert!(matches!(
        cli.reply().await,
        CliReply::Error(e) if e.code == ProtocolErrorCode::Unavailable
    ));
}
