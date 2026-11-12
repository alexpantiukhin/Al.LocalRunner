mod common;

use common::broker::{FakeBroker, Script};
use common::*;
use lr_core::JobId;
use lr_proto::{
    BrokerToHub, CliRead, CliReply, CliRequest, HubToBroker, LogChunk, ProtocolErrorCode,
    ReadAnswer, ReadReply, ReadRequest, Topic,
};

fn log(follow: bool) -> CliRequest {
    CliRequest::Read(CliRead {
        broker: Some("local".into()),
        request: ReadRequest::Log {
            job: JobId("j1".into()),
            step: None,
            tail: None,
            follow,
            client: None,
        },
    })
}

async fn connected(rig: &mut Rig) -> FakeBroker {
    started(rig).await.0
}

async fn started(rig: &mut Rig) -> (FakeBroker, tokio::task::JoinHandle<()>) {
    let local = rig.local_broker(Script::local_runner());
    let hub = rig.start();
    eventually("hub connects", || local.hellos() == 1).await;
    settle().await;
    (local, hub)
}

fn hubs_own_subscription(r: &ReadRequest) -> bool {
    matches!(r, ReadRequest::Subscribe { topics } if topics == &[Topic::Queue, Topic::Runners])
}

fn reads(b: &FakeBroker) -> Vec<(u64, ReadRequest)> {
    b.received()
        .into_iter()
        .filter_map(|m| match m {
            HubToBroker::Read(c) if !hubs_own_subscription(&c.request) => Some((c.req, c.request)),
            _ => None,
        })
        .collect()
}

async fn asked(b: &FakeBroker) -> u64 {
    eventually("the read reaches the broker", || !reads(b).is_empty()).await;
    reads(b)[0].0
}

fn chunk(req: u64, text: &str, end: bool) -> BrokerToHub {
    BrokerToHub::LogChunk(LogChunk {
        req,
        text: text.into(),
        end,
    })
}

#[tokio::test]
async fn log_read_relays_the_brokers_chunks_in_order() {
    let mut rig = Rig::new();
    let local = connected(&mut rig).await;
    let cli = rig.cli().await;
    cli.send(log(false)).await;
    let req = asked(&local).await;
    local.send(chunk(req, "a\n", false)).await;
    local.send(chunk(req, "b\n", true)).await;
    assert_eq!(
        cli.reply().await,
        CliReply::LogChunk {
            broker: "local".into(),
            text: "a\n".into(),
            end: false
        }
    );
    assert_eq!(
        cli.reply().await,
        CliReply::LogChunk {
            broker: "local".into(),
            text: "b\n".into(),
            end: true
        }
    );
}

#[tokio::test]
async fn a_log_refusal_comes_back_as_a_read_answer() {
    let mut rig = Rig::new();
    let local = connected(&mut rig).await;
    let cli = rig.cli().await;
    cli.send(log(false)).await;
    let req = asked(&local).await;
    let denied = ReadReply::Denied {
        reason: "not yours".into(),
    };
    local
        .send(BrokerToHub::ReadReply(ReadAnswer {
            req,
            reply: denied.clone(),
        }))
        .await;
    assert_eq!(
        cli.reply().await,
        CliReply::Read {
            broker: "local".into(),
            reply: denied
        }
    );
}

#[tokio::test]
async fn a_followed_log_ends_with_an_error_when_the_broker_goes() {
    let mut rig = Rig::new();
    let local = connected(&mut rig).await;
    let cli = rig.cli().await;
    cli.send(log(true)).await;
    let req = asked(&local).await;
    local.send(chunk(req, "a\n", false)).await;
    assert!(matches!(
        cli.reply().await,
        CliReply::LogChunk { end: false, .. }
    ));
    local.disconnect().await;
    match cli.reply().await {
        CliReply::Error(e) => assert_eq!(e.code, ProtocolErrorCode::Unavailable, "{e:?}"),
        other => panic!("expected an error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_followed_log_waits_beyond_the_query_timeout() {
    let mut rig = Rig::new();
    let local = connected(&mut rig).await;
    let cli = rig.cli().await;
    cli.send(log(true)).await;
    let req = asked(&local).await;
    rig.step(30).await;
    local.send(chunk(req, "late\n", false)).await;
    assert_eq!(
        cli.reply().await,
        CliReply::LogChunk {
            broker: "local".into(),
            text: "late\n".into(),
            end: false
        }
    );
}

#[tokio::test]
async fn a_viewer_that_leaves_unfollows_the_log() {
    let mut rig = Rig::new();
    let local = connected(&mut rig).await;
    let cli = rig.cli().await;
    cli.send(log(true)).await;
    let req = asked(&local).await;
    local.send(chunk(req, "a\n", false)).await;
    assert!(matches!(cli.reply().await, CliReply::LogChunk { .. }));
    cli.close().await;
    eventually("the hub unfollows", || {
        reads(&local)
            .iter()
            .any(|(_, r)| *r == ReadRequest::Unfollow { req })
    })
    .await;
}

#[tokio::test]
async fn a_followed_log_that_falls_behind_ends_and_unfollows() {
    let mut rig = Rig::new();
    let local = connected(&mut rig).await;
    let cli = rig.cli().await;
    cli.send(log(true)).await;
    let req = asked(&local).await;
    for i in 0..5000 {
        local.send(chunk(req, &format!("{i}\n"), false)).await;
    }
    let error = loop {
        match cli.reply().await {
            CliReply::LogChunk { end: false, .. } => continue,
            CliReply::Error(e) => break e,
            other => panic!("expected chunks then an error, got {other:?}"),
        }
    };
    assert_eq!(error.code, ProtocolErrorCode::Unavailable, "{error:?}");
    eventually("the hub unfollows the lagging log", || {
        reads(&local)
            .iter()
            .any(|(_, r)| *r == ReadRequest::Unfollow { req })
    })
    .await;
}

#[tokio::test]
async fn the_cli_cannot_manage_the_hubs_subscriptions() {
    let mut rig = Rig::new();
    let local = connected(&mut rig).await;
    let cli = rig.cli().await;
    for request in [
        ReadRequest::Unfollow { req: 1 },
        ReadRequest::Unsubscribe { sub: 1 },
        ReadRequest::Subscribe { topics: Vec::new() },
    ] {
        cli.send(CliRequest::Read(CliRead {
            broker: None,
            request,
        }))
        .await;
        match cli.reply().await {
            CliReply::Error(e) => assert_eq!(e.code, ProtocolErrorCode::Malformed, "{e:?}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
    settle().await;
    assert!(reads(&local).is_empty(), "{:?}", reads(&local));
}

#[tokio::test]
async fn shutdown_stops_the_hub() {
    let mut rig = Rig::new();
    let (_local, hub) = started(&mut rig).await;
    let cli = rig.cli().await;
    cli.send(CliRequest::Shutdown).await;
    assert_eq!(cli.reply().await, CliReply::Stopping);
    eventually("the hub ends", || hub.is_finished()).await;
}

async fn with_a_second_broker(rig: &mut Rig) -> (FakeBroker, FakeBroker) {
    rig.join("far").await;
    let far = rig.broker(
        "far",
        lr_core::Zone::Remote,
        Script::linux(lr_core::Zone::Remote),
    );
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    eventually("both brokers connect", || {
        local.hellos() == 1 && far.hellos() == 1
    })
    .await;
    settle().await;
    (local, far)
}

#[tokio::test]
async fn another_brokers_answer_to_a_request_number_is_dropped() {
    let mut rig = Rig::new();
    let (local, far) = with_a_second_broker(&mut rig).await;
    let cli = rig.cli().await;
    cli.send(CliRequest::Read(CliRead {
        broker: Some("local".into()),
        request: ReadRequest::HistoryShow {
            job: JobId("j1".into()),
            client: None,
        },
    }))
    .await;
    let req = asked(&local).await;
    let forged = ReadReply::Denied {
        reason: "forged".into(),
    };
    far.send(BrokerToHub::ReadReply(ReadAnswer { req, reply: forged }))
        .await;
    settle().await;
    let real = ReadReply::Denied {
        reason: "from local".into(),
    };
    local
        .send(BrokerToHub::ReadReply(ReadAnswer {
            req,
            reply: real.clone(),
        }))
        .await;
    assert_eq!(
        cli.reply().await,
        CliReply::Read {
            broker: "local".into(),
            reply: real
        }
    );
}

#[tokio::test]
async fn another_brokers_log_chunks_never_reach_the_stream() {
    let mut rig = Rig::new();
    let (local, far) = with_a_second_broker(&mut rig).await;
    let cli = rig.cli().await;
    cli.send(log(false)).await;
    let req = asked(&local).await;
    far.send(chunk(req, "forged\n", true)).await;
    settle().await;
    local.send(chunk(req, "real\n", true)).await;
    assert_eq!(
        cli.reply().await,
        CliReply::LogChunk {
            broker: "local".into(),
            text: "real\n".into(),
            end: true
        }
    );
}
