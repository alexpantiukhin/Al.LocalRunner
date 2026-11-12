mod common;

use common::broker::{OnOrder, Script};
use common::*;
use lr_core::Zone;
use lr_proto::{CliReply, ReasonCode, RejectReason};

fn lan_only(rig: &Rig) {
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

#[tokio::test]
async fn rejection_with_no_allowed_broker_left_reports_the_last_reason() {
    let mut rig = Rig::new();
    lan_only(&rig);
    rig.join("lan-a").await;
    rig.join("far").await;
    let no_runner = ReasonCode::Rejected(RejectReason::NoMatchingRunner);
    let lan = rig.broker(
        "lan-a",
        Zone::Lan,
        Script::linux(Zone::Lan).on_order(OnOrder::Reject(no_runner.clone(), "busy".into())),
    );
    let far = rig.broker("far", Zone::Remote, Script::linux(Zone::Remote));
    rig.start();
    eventually("lan connected", || lan.resumed()).await;
    eventually("far connected", || far.resumed()).await;
    settle().await;
    let cli = rig.cli().await;
    let mut req = submit("remote", Some("linux"));
    req.wait = true;
    cli.submit(req).await;
    match cli.reply().await {
        CliReply::Rejected { code, detail } => {
            assert_eq!(code, no_runner);
            assert!(detail.contains("busy"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(far.queries(), 0);
}

#[tokio::test]
async fn broker_choice_retried_when_recording_it_fails() {
    let mut rig = Rig::new();
    rig.join("lan-a").await;
    let lan = rig.broker("lan-a", Zone::Lan, Script::linux(Zone::Lan));
    rig.start();
    eventually("connected", || lan.resumed()).await;
    settle().await;
    rig.store.fail("set_broker", 1);
    let job = rig
        .cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    settle().await;
    assert!(lan.orders().is_empty(), "the choice was not recorded");
    rig.step(4).await;
    assert!(lan.orders().is_empty(), "retried after query_retry_s = 5");
    rig.step(1).await;
    eventually("placed on retry", || lan.orders().len() == 1).await;
    settle().await;
    assert_eq!(
        rig.active(&job).await.unwrap().broker.as_deref(),
        Some("lan-a")
    );
    assert!(rig.log_text().contains("broker choice not recorded"));
}
