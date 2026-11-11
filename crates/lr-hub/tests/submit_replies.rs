mod common;

use common::broker::{caps, OnOrder, Script};
use common::*;
use lr_core::Zone;
use lr_proto::{Accepted, BrokerToHub, CliReply, CliRequest, ProtocolErrorCode};

#[tokio::test]
async fn several_platforms_without_platform_flag_ask_to_choose_one() {
    let mut rig = Rig::new();
    let profiles = PROFILES.replace(
        "platforms = [\"linux\"]",
        "platforms = [\"windows\", \"linux\"]",
    );
    rig.repo
        .commit_file(COMMIT, ".localrunner/profiles.toml", profiles.as_bytes());
    rig.files.put(
        &repo_root().join(".localrunner").join("profiles.toml"),
        profiles.as_bytes(),
    );
    rig.join("lan1").await;
    let mut script = Script::linux(Zone::Lan);
    script.runners[0].caps = caps(&["linux", "windows"], None);
    let lan = rig.broker("lan1", Zone::Lan, script);
    rig.start();
    eventually("connected", || lan.resumed()).await;
    let cli = rig.cli().await;
    cli.send(CliRequest::Submit(submit("remote", None))).await;
    match cli.reply().await {
        CliReply::Error(e) => {
            assert_eq!(e.code, ProtocolErrorCode::Malformed);
            assert_eq!(
                e.message,
                "profile `remote` lists several platforms (linux, windows); choose one with --platform"
            );
        }
        other => panic!("{other:?}"),
    }
    settle().await;
    assert!(rig.pending().await.is_empty(), "no order, no JobId");
    assert!(lan.orders().is_empty());
    let job = cli.submit(submit("remote", Some("windows"))).await;
    eventually("order sent", || lan.orders().len() == 1).await;
    assert_eq!(lan.orders()[0].job, job);
    assert_eq!(
        lan.orders()[0].spec.requirements.platform.as_deref(),
        Some("windows")
    );
}

#[tokio::test]
async fn trimmed_timeout_line_reaches_the_waiting_cli() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner().on_order(OnOrder::Silent));
    rig.start();
    let cli = rig.cli().await;
    let mut req = submit("unit", None);
    req.wait = true;
    let job = cli.submit(req).await;
    eventually("order reaches the broker", || local.orders().len() == 1).await;
    local
        .send(BrokerToHub::Accepted(Accepted {
            job: job.clone(),
            order_seq: 1,
            waiting: None,
            timeout_trimmed_to: Some(300),
        }))
        .await;
    assert_eq!(
        cli.reply().await,
        CliReply::Output {
            job,
            text: "=== timeout trimmed by the broker policy: 600 s -> 300 s ===\n".into()
        }
    );
}
