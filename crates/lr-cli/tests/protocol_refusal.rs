use lr_abstractions::fakes::{channel_pair, ManualClock};
use lr_abstractions::{Frame, Peer, PeerAccess, Transport};
use lr_cli::commands::down_cmd::not_reached;
use lr_cli::ipc_client::{protocol_refusal, IpcClient};
use lr_core::{Endpoint, Timestamp};
use lr_proto::{IpcChannel, IpcOpenReply, ProtocolError, ProtocolErrorCode, Version};
use std::sync::Arc;

async fn refusal_of(error: ProtocolError) -> String {
    let peer = || Peer {
        endpoint: Endpoint::Ipc {
            os_user: "alex".into(),
        },
        key: None,
        access: PeerAccess::Local,
    };
    let clock = Arc::new(ManualClock::new(Timestamp(0)));
    let (cli, role) = channel_pair(clock, peer(), peer());
    tokio::spawn(async move {
        let _hello = role.recv().await.expect("hello");
        let reply = IpcOpenReply::Refused(error);
        role.send(Frame::json(&reply).unwrap()).await.unwrap();
    });
    match IpcClient::open(Box::new(cli), IpcChannel::Cli).await {
        Err(e) => e,
        Ok(_) => panic!("the role refused the channel"),
    }
}

fn unsupported(supported: Option<Version>) -> ProtocolError {
    ProtocolError {
        code: ProtocolErrorCode::UnsupportedVersion,
        message: "protocol 2.0 is not supported".into(),
        supported,
    }
}

#[tokio::test]
async fn a_role_on_an_older_protocol_is_named_with_the_broker_service_fix() {
    let e = refusal_of(unsupported(Some(Version { major: 1, minor: 0 }))).await;
    assert!(protocol_refusal(&e), "{e}");
    for part in [
        "protocol 2.0 is not supported",
        "answers on protocol 1.0",
        "this binary speaks 2.0",
        "broker update-service",
        "localrunner up",
    ] {
        assert!(e.contains(part), "{part}: {e}");
    }
}

#[tokio::test]
async fn a_role_on_a_newer_protocol_sends_to_upgrade_and_one_without_a_version_still_refuses() {
    let e = refusal_of(unsupported(Some(Version { major: 3, minor: 1 }))).await;
    assert!(protocol_refusal(&e), "{e}");
    assert!(e.contains("answers on protocol 3.1"), "{e}");
    assert!(e.contains("localrunner upgrade"), "{e}");
    assert!(!e.contains("update-service"), "{e}");
    let e = refusal_of(unsupported(None)).await;
    assert!(protocol_refusal(&e), "{e}");
    assert!(e.contains("another protocol"), "{e}");
}

#[tokio::test]
async fn another_refusal_stays_as_the_role_said_it() {
    let e = refusal_of(ProtocolError {
        code: ProtocolErrorCode::Malformed,
        message: "this is the Cli channel".into(),
        supported: None,
    })
    .await;
    assert_eq!(e, "refused: this is the Cli channel");
    assert!(!protocol_refusal(&e));
}

#[test]
fn down_reports_a_role_on_the_old_protocol_instead_of_not_running() {
    let e = not_reached(
        "refused: protocol 2.0 is not supported; the role answers on protocol 1.0, this binary speaks 2.0: x",
    )
    .unwrap_err();
    assert!(
        e.contains("old protocol") && e.contains("answers on protocol 1.0"),
        "{e}"
    );
    assert_eq!(
        not_reached("/run/x.sock: unreachable: No such file (os error 2)"),
        Ok(false),
        "a role that does not listen is not running"
    );
}
