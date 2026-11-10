mod common;

use common::broker::{caps, OnOrder, Script};
use common::*;
use lr_abstractions::HubStore;
use lr_core::{JobState, Zone};
use lr_hub::{BrokerStrategy, ZoneQuery};
use lr_proto::{CliReply, ProtocolError, ProtocolErrorCode, ReasonCode, RejectReason, RunnerCaps};

fn no_runner() -> ReasonCode {
    ReasonCode::Rejected(RejectReason::NoMatchingRunner)
}

async fn wait_connected(brokers: &[&common::broker::FakeBroker]) {
    for b in brokers {
        eventually("broker connected", || b.resumed()).await;
    }
    settle().await;
}

#[tokio::test]
async fn broker_choice_follows_order_and_zone_preference() {
    let mut rig = Rig::new();
    rig.policy.brokers.order = vec!["far".into(), "lan-b".into(), "lan-a".into()];
    for name in ["far", "lan-a", "lan-b"] {
        rig.join(name).await;
    }
    let far = rig.broker("far", Zone::Remote, Script::linux(Zone::Remote));
    let lan_a = rig.broker("lan-a", Zone::Lan, Script::linux(Zone::Lan));
    let lan_b = rig.broker("lan-b", Zone::Lan, Script::linux(Zone::Lan));
    rig.start();
    wait_connected(&[&far, &lan_a, &lan_b]).await;
    rig.cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    eventually("order placed", || lan_b.orders().len() == 1).await;
    settle().await;
    assert!(
        lan_a.orders().is_empty(),
        "lan-b comes first in [brokers] order"
    );
    assert!(
        far.orders().is_empty(),
        "the nearer zone wins over the order"
    );
    assert_eq!(far.queries(), 0, "the next zone is not queried");
}

#[tokio::test]
async fn reversed_zone_order_prefers_remote() {
    let mut rig = Rig::new();
    rig.policy.brokers.broker_zone_order = vec![Zone::Remote, Zone::Lan, Zone::Local];
    rig.join("far").await;
    rig.join("lan-a").await;
    let far = rig.broker("far", Zone::Remote, Script::linux(Zone::Remote));
    let lan = rig.broker("lan-a", Zone::Lan, Script::linux(Zone::Lan));
    rig.start();
    wait_connected(&[&far, &lan]).await;
    rig.cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    eventually("order placed", || far.orders().len() == 1).await;
    assert_eq!(lan.queries(), 0);
}

#[tokio::test]
async fn next_zone_when_nearer_has_no_suitable_runner() {
    let mut rig = Rig::new();
    rig.join("far").await;
    rig.join("lan-a").await;
    let far = rig.broker("far", Zone::Remote, Script::linux(Zone::Remote));
    let mut s = Script::linux(Zone::Lan);
    s.runners[0].caps = caps(&["windows"], None);
    let lan = rig.broker("lan-a", Zone::Lan, s);
    rig.start();
    wait_connected(&[&far, &lan]).await;
    rig.cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    eventually("order placed", || far.orders().len() == 1).await;
    assert_eq!(lan.queries(), 1);
    assert!(lan.orders().is_empty());
}

#[tokio::test]
async fn offline_runner_does_not_count_as_suitable() {
    let mut rig = Rig::new();
    rig.join("far").await;
    rig.join("lan-a").await;
    let far = rig.broker("far", Zone::Remote, Script::linux(Zone::Remote));
    let mut s = Script::linux(Zone::Lan);
    s.runners[0].online = false;
    let lan = rig.broker("lan-a", Zone::Lan, s);
    rig.start();
    wait_connected(&[&far, &lan]).await;
    rig.cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    eventually("order placed", || far.orders().len() == 1).await;
    assert!(lan.orders().is_empty());
}

#[tokio::test]
async fn zone_limited_order_never_queries_remote() {
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
    rig.join("far").await;
    let far = rig.broker("far", Zone::Remote, Script::linux(Zone::Remote));
    rig.start();
    wait_connected(&[&far]).await;
    let cli = rig.cli().await;
    let mut req = submit("remote", Some("linux"));
    req.wait = true;
    cli.submit(req).await;
    match cli.reply().await {
        CliReply::Rejected { code, .. } => {
            assert_eq!(code, ReasonCode::Rejected(RejectReason::ZoneNotServed))
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(far.queries(), 0);
}

#[tokio::test]
async fn all_zones_queried_at_once_with_zone_query_all() {
    let mut rig = Rig::new();
    rig.policy.brokers.zone_query = ZoneQuery::All;
    rig.join("far").await;
    rig.join("lan-a").await;
    let far = rig.broker("far", Zone::Remote, Script::linux(Zone::Remote));
    let lan = rig.broker("lan-a", Zone::Lan, Script::linux(Zone::Lan));
    rig.start();
    wait_connected(&[&far, &lan]).await;
    rig.cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    eventually("order placed", || lan.orders().len() == 1).await;
    assert_eq!(far.queries(), 1, "every allowed zone is queried");
    assert!(
        far.orders().is_empty(),
        "the choice still follows the zone order"
    );
}

#[tokio::test]
async fn least_loaded_prefers_more_suitable_runners() {
    let mut rig = Rig::new();
    rig.policy.brokers.strategy = BrokerStrategy::LeastLoaded;
    rig.join("lan-a").await;
    rig.join("lan-b").await;
    let a = rig.broker("lan-a", Zone::Lan, Script::linux(Zone::Lan));
    let mut s = Script::linux(Zone::Lan);
    s.runners.push(RunnerCaps {
        name: "r2".into(),
        online: true,
        zone: Zone::Lan,
        caps: caps(&["linux"], None),
    });
    let b = rig.broker("lan-b", Zone::Lan, s);
    rig.start();
    wait_connected(&[&a, &b]).await;
    rig.cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    eventually("order placed", || b.orders().len() == 1).await;
    assert!(a.orders().is_empty());
}

#[tokio::test]
async fn rejected_ordinary_order_tries_next_broker() {
    let mut rig = Rig::new();
    rig.policy.brokers.order = vec!["lan-a".into(), "lan-b".into()];
    rig.join("lan-a").await;
    rig.join("lan-b").await;
    let a = rig.broker(
        "lan-a",
        Zone::Lan,
        Script::linux(Zone::Lan).on_order(OnOrder::Reject(no_runner(), "busy".into())),
    );
    let b = rig.broker("lan-b", Zone::Lan, Script::linux(Zone::Lan));
    rig.start();
    wait_connected(&[&a, &b]).await;
    let job = rig
        .cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    eventually("tried the next broker", || b.orders().len() == 1).await;
    assert_eq!(a.orders().len(), 1);
    assert_eq!(
        b.orders()[0].seq,
        1,
        "the stream restarts at the new broker"
    );
    settle().await;
    assert!(rig.pending().await.is_empty());
    assert_eq!(
        rig.active(&job).await.unwrap().broker.as_deref(),
        Some("lan-b")
    );
}

#[tokio::test]
async fn rejection_by_every_broker_reaches_the_waiting_cli() {
    let mut rig = Rig::new();
    rig.join("lan-a").await;
    let a = rig.broker(
        "lan-a",
        Zone::Lan,
        Script::linux(Zone::Lan).on_order(OnOrder::Reject(no_runner(), "12288 > 8192".into())),
    );
    rig.start();
    wait_connected(&[&a]).await;
    let cli = rig.cli().await;
    let mut req = submit("remote", Some("linux"));
    req.wait = true;
    let job = cli.submit(req).await;
    match cli.reply().await {
        CliReply::Rejected { code, detail } => {
            assert_eq!(code, no_runner());
            assert!(detail.contains("12288 > 8192"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
    assert!(rig.active(&job).await.is_none());
}

#[tokio::test]
async fn same_machine_rejection_does_not_search_replacement() {
    let mut rig = Rig::new();
    rig.join("lan-a").await;
    let local = rig.local_broker(Script::local_runner().on_order(OnOrder::Reject(
        ReasonCode::Rejected(RejectReason::NoRunnerOnMachine),
        "no runner".into(),
    )));
    let lan = rig.broker("lan-a", Zone::Lan, Script::linux(Zone::Lan));
    rig.start();
    wait_connected(&[&local, &lan]).await;
    let cli = rig.cli().await;
    let mut req = submit("unit", None);
    req.wait = true;
    let job = cli.submit(req).await;
    match cli.reply().await {
        CliReply::Rejected { code, .. } => {
            assert_eq!(code, ReasonCode::Rejected(RejectReason::NoRunnerOnMachine))
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(local.orders().len(), 1);
    assert_eq!(lan.queries(), 0);
    assert!(lan.orders().is_empty());
    assert!(rig.active(&job).await.is_none());
}

#[tokio::test]
async fn returned_same_machine_order_ends_with_hint() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    wait_connected(&[&local]).await;
    let cli = rig.cli().await;
    let mut req = submit("unit", None);
    req.wait = true;
    let job = cli.submit(req).await;
    eventually("accepted", || local.orders().len() == 1).await;
    settle().await;
    local
        .returned(&job, ReasonCode::Rejected(RejectReason::NoRunnerOnMachine))
        .await;
    match cli.reply().await {
        CliReply::Rejected { code, detail } => {
            assert_eq!(code, ReasonCode::Rejected(RejectReason::NoRunnerOnMachine));
            assert!(detail.contains("up"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(local.orders().len(), 1);
}

#[tokio::test]
async fn returned_ordinary_order_goes_to_another_broker_at_seq_one() {
    let mut rig = Rig::new();
    rig.policy.brokers.order = vec!["lan-a".into(), "lan-b".into()];
    rig.join("lan-a").await;
    rig.join("lan-b").await;
    let a = rig.broker("lan-a", Zone::Lan, Script::linux(Zone::Lan));
    let b = rig.broker("lan-b", Zone::Lan, Script::linux(Zone::Lan));
    rig.start();
    wait_connected(&[&a, &b]).await;
    let job = rig
        .cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    eventually("accepted by a", || a.orders().len() == 1).await;
    settle().await;
    a.returned(&job, ReasonCode::RunnerRemoved).await;
    eventually("replacement at b", || b.orders().len() == 1).await;
    assert_eq!(b.orders()[0].seq, 1);
    assert_eq!(
        b.orders()[0].spec.enqueued_at.0,
        START,
        "age survives re-brokering"
    );
    settle().await;
    b.started(&job).await;
    settle().await;
    assert_eq!(
        rig.active(&job).await.unwrap().last_seq,
        1,
        "b's stream starts at 1"
    );
    b.result(&job, JobState::Passed, Some(0)).await;
    settle().await;
    assert!(rig.active(&job).await.is_none());
}

#[tokio::test]
async fn unavailable_broker_retries_then_rejects_naming_it() {
    let mut rig = Rig::new();
    rig.policy.brokers.query_attempts = 2;
    rig.policy.brokers.query_retry_s = 5;
    rig.join("lan-a").await;
    rig.join("lan-b").await;
    let mut silent = Script::linux(Zone::Lan);
    silent.answer_queries = false;
    let a = rig.broker("lan-a", Zone::Lan, silent);
    let mut other = Script::linux(Zone::Lan);
    other.runners[0].caps = caps(&["windows"], None);
    let b = rig.broker("lan-b", Zone::Lan, other);
    rig.start();
    wait_connected(&[&a, &b]).await;
    let cli = rig.cli().await;
    let mut req = submit("remote", Some("linux"));
    req.wait = true;
    cli.submit(req).await;
    rig.step(3).await;
    assert_eq!(a.queries(), 1);
    rig.step(4).await;
    assert_eq!(
        a.queries(),
        1,
        "pause query_retry_s before the next attempt"
    );
    rig.step(1).await;
    assert_eq!(a.queries(), 2);
    rig.step(3).await;
    match cli.reply().await {
        CliReply::Rejected { code, detail } => {
            assert_eq!(code, ReasonCode::Rejected(RejectReason::BrokersUnreachable));
            assert!(detail.contains("lan-a"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
    assert!(a.orders().is_empty() && b.orders().is_empty());
}

#[tokio::test]
async fn mismatch_rejection_with_figures_when_all_answered() {
    let mut rig = Rig::new();
    rig.join("lan-a").await;
    let mut s = Script::linux(Zone::Lan);
    s.runners[0].caps.mem_mb = 1000;
    let a = rig.broker("lan-a", Zone::Lan, s);
    rig.start();
    wait_connected(&[&a]).await;
    let cli = rig.cli().await;
    let mut req = submit("remote", Some("linux"));
    req.wait = true;
    cli.submit(req).await;
    match cli.reply().await {
        CliReply::Rejected { code, detail } => {
            assert_eq!(code, no_runner());
            assert!(
                detail.contains("2048") && detail.contains("1000"),
                "{detail}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn no_broker_answering_keeps_order_waiting_until_connection() {
    let mut rig = Rig::new();
    rig.join("lan-a").await;
    rig.net.set_reachable("lan-a:7443", false);
    let a = rig.broker("lan-a", Zone::Lan, Script::linux(Zone::Lan));
    rig.net.set_reachable("lan-a:7443", false);
    rig.store.set_zone("lan-a", Zone::Lan).await.unwrap();
    rig.start();
    let job = rig
        .cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    rig.step(60).await;
    assert!(
        rig.active(&job).await.is_some(),
        "waits instead of a rejection"
    );
    assert_eq!(rig.pending().await.len(), 1);
    rig.net.set_reachable("lan-a:7443", true);
    rig.step(40).await;
    eventually("placed once the broker is back", || a.orders().len() == 1).await;
}

#[tokio::test]
async fn same_machine_without_local_broker_is_rejected() {
    let mut rig = Rig::new();
    rig.start();
    let cli = rig.cli().await;
    cli.send(lr_proto::CliRequest::Submit(submit("unit", None)))
        .await;
    match cli.reply().await {
        CliReply::Rejected { code, .. } => {
            assert_eq!(code, ReasonCode::Rejected(RejectReason::NoLocalBroker))
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn local_user_refused_by_broker_rejects_same_machine_order() {
    let mut rig = Rig::new();
    let mut s = Script::local_runner();
    s.refuse = Some(ProtocolError {
        code: ProtocolErrorCode::Unauthorized,
        message: "OS user alex is not in [[local_user]]".into(),
        supported: None,
    });
    let _local = rig.local_broker(s);
    rig.start();
    let cli = rig.cli().await;
    let mut req = submit("unit", None);
    req.wait = true;
    cli.submit(req).await;
    rig.step(2).await;
    match cli.reply().await {
        CliReply::Rejected { code, .. } => {
            assert_eq!(code, ReasonCode::Rejected(RejectReason::LocalUserUnknown))
        }
        other => panic!("{other:?}"),
    }
}
