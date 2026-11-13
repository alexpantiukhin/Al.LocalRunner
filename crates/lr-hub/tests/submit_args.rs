mod common;

use common::broker::Script;
use common::*;
use lr_core::Zone;
use lr_proto::{CliReply, CliRequest, ProtocolErrorCode};

fn with_args(
    profile: &str,
    platform: Option<&str>,
    args: &[(&str, &str)],
) -> lr_proto::SubmitRequest {
    let mut req = submit(profile, platform);
    req.args = args
        .iter()
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect();
    req
}

#[tokio::test]
async fn arg_fills_the_command_and_is_recorded() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    cli.submit(with_args(
        "targeted",
        None,
        &[("target", "alpha"), ("pkg", "core")],
    ))
    .await;
    cli.submit(with_args("targeted", None, &[])).await;
    eventually("orders sent", || local.orders().len() == 2).await;
    let first = &local.orders()[0].spec;
    assert_eq!(first.steps[0].command, "cargo test -p core alpha");
    assert_eq!(
        first.args,
        vec![
            ("target".to_string(), "alpha".to_string()),
            ("pkg".to_string(), "core".to_string())
        ]
    );
    let second = &local.orders()[1].spec;
    assert_eq!(second.steps[0].command, "cargo test all");
    assert!(second.args.is_empty());
}

#[tokio::test]
async fn commit_order_gets_substitution_too() {
    let mut rig = Rig::new();
    rig.join("lan1").await;
    let lan = rig.broker("lan1", Zone::Lan, Script::linux(Zone::Lan));
    rig.start();
    eventually("connected", || lan.resumed()).await;
    rig.cli()
        .await
        .submit(with_args(
            "targeted-remote",
            Some("linux"),
            &[("target", "beta")],
        ))
        .await;
    eventually("order sent", || lan.orders().len() == 1).await;
    assert_eq!(lan.orders()[0].spec.steps[0].command, "cargo test beta");
}

#[tokio::test]
async fn bad_arg_is_malformed_and_nothing_is_sent() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    let cases = [
        (with_args("targeted", None, &[("nope", "1")]), "nope"),
        (with_args("targeted", None, &[("target", "gamma")]), "gamma"),
        (with_args("targeted", None, &[("pkg", "Bad_1")]), "pkg"),
        (
            with_args("targeted", None, &[("target", "a"), ("target", "b")]),
            "target",
        ),
        (with_args("unit", None, &[("target", "a")]), "none"),
    ];
    for (req, word) in cases {
        cli.send(CliRequest::Submit(req)).await;
        match cli.reply().await {
            CliReply::Error(e) => {
                assert_eq!(e.code, ProtocolErrorCode::Malformed);
                assert!(e.message.contains(word), "{}", e.message);
            }
            other => panic!("{other:?}"),
        }
    }
    settle().await;
    assert!(local.orders().is_empty());
}

#[tokio::test]
async fn args_do_not_change_requirements() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    cli.submit(with_args("targeted", None, &[("target", "alpha")]))
        .await;
    let mut big = with_args("targeted", None, &[("target", "beta")]);
    big.mem_mb = Some(4096);
    cli.submit(big).await;
    eventually("orders sent", || local.orders().len() == 2).await;
    assert_eq!(local.orders()[0].spec.requirements.mem_mb, 1024);
    assert_eq!(local.orders()[1].spec.requirements.mem_mb, 4096);
}

#[tokio::test]
async fn profile_without_args_is_unchanged() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    rig.cli().await.submit(submit("unit", None)).await;
    eventually("order sent", || local.orders().len() == 1).await;
    let spec = &local.orders()[0].spec;
    assert_eq!(spec.steps[0].command, "make test");
    assert!(spec.args.is_empty());
}
