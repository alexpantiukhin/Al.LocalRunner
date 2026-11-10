mod common;

use common::*;
use lr_abstractions::{Dialer, Frame, Listener, Peer, PeerAccess, Transport, TransportError};
use lr_core::{Endpoint, Site, Zone};
use lr_net::{
    current_os_user, peer_kind, IpcAccess, IpcDialer, IpcListener, LocalUser, LocalUsers,
    ServerIdentity, PREAMBLE_TIMEOUT,
};
use lr_proto::{decode_for_peer, AdminRequest, PeerKind, RejectReason};
use std::collections::BTreeMap;
use std::time::Duration;

fn address() -> String {
    let name = unique("lr-test");
    if cfg!(windows) {
        format!(r"\\.\pipe\{name}")
    } else {
        std::env::temp_dir()
            .join(format!("{name}.sock"))
            .to_string_lossy()
            .into_owned()
    }
}

fn dialer() -> IpcDialer {
    IpcDialer::new(ServerIdentity::current_user().expect("own identity"))
}

async fn pair(access: IpcAccess) -> (IpcListener, Box<dyn Transport>, Box<dyn Transport>) {
    let addr = address();
    let listener = IpcListener::bind(&addr, access).await.expect("bind");
    let client = dialer().dial(&addr, None).await.expect("dial");
    let server = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("accepted in time")
        .expect("accept");
    (listener, client, server)
}

#[tokio::test]
async fn ipc_peer_mapped_to_local_user() {
    let me = current_os_user().expect("own user");
    let (_l, client, server) = pair(IpcAccess::OwnerOnly).await;
    assert_eq!(
        server.peer(),
        &Peer {
            endpoint: Endpoint::Ipc {
                os_user: me.clone()
            },
            key: None,
            access: PeerAccess::Local,
        }
    );
    let users = LocalUsers::new(vec![
        LocalUser {
            name: "someone-else".into(),
            label: String::new(),
        },
        LocalUser {
            name: me.clone(),
            label: "ci".into(),
        },
    ]);
    let local = users.admit(server.peer()).expect("listed user admitted");
    assert_eq!(local.os_user, me);
    assert_eq!(local.label, "ci");
    assert_eq!(local.client_key, format!("local:{me}"));
    let unlisted = LocalUsers::new(vec![LocalUser {
        name: "someone-else".into(),
        label: String::new(),
    }]);
    assert_eq!(
        unlisted.admit(server.peer()),
        Err(RejectReason::LocalUserUnknown)
    );
    exchange(client.as_ref(), server.as_ref()).await;
    exchange(server.as_ref(), client.as_ref()).await;
}

#[tokio::test]
async fn foreign_os_user_gets_broker_host_zone() {
    let (_l, _client, server) = pair(IpcAccess::Principals(vec![current_os_user().unwrap()])).await;
    let runner = server.peer().endpoint.clone();
    let foreign = Endpoint::Ipc {
        os_user: r"CORP\another-os-user".into(),
    };
    let mut networks = BTreeMap::new();
    networks.insert("home".to_string(), vec!["192.168.1.0/24".parse().unwrap()]);
    let site = Site {
        broker_addrs: vec!["192.168.1.10".parse().unwrap()],
        networks,
    };
    assert_eq!(site.zone_between(&runner, &runner), Zone::Local);
    assert_eq!(
        site.zone_between(&foreign, &runner),
        Zone::Lan,
        "IPC of another OS user counts as the broker host's address"
    );
    let lan_host = Endpoint::Addr("192.168.1.10".parse().unwrap());
    assert_eq!(
        site.zone_between(&foreign, &lan_host),
        site.zone_between(&lan_host, &lan_host)
    );
    let remote = Endpoint::Addr("10.9.9.9".parse().unwrap());
    assert_eq!(site.zone_between(&foreign, &remote), Zone::Remote);
    let users = LocalUsers::new(vec![
        LocalUser {
            name: r"CORP\another-os-user".into(),
            label: "".into(),
        },
        LocalUser {
            name: current_os_user().unwrap(),
            label: "".into(),
        },
    ]);
    let a = users
        .admit(&Peer {
            endpoint: foreign,
            key: None,
            access: PeerAccess::Local,
        })
        .unwrap();
    let b = users.admit(server.peer()).unwrap();
    assert_ne!(a.client_key, b.client_key, "own client record per OS user");
}

#[tokio::test]
async fn network_peer_is_never_a_local_user() {
    let users = LocalUsers::new(vec![LocalUser {
        name: current_os_user().unwrap(),
        label: "".into(),
    }]);
    let net = Peer {
        endpoint: Endpoint::Addr("127.0.0.1".parse().unwrap()),
        key: None,
        access: PeerAccess::Member,
    };
    assert_eq!(users.admit(&net), Err(RejectReason::LocalUserUnknown));
    assert_eq!(peer_kind(&net), PeerKind::Network);
}

#[tokio::test]
async fn admin_commands_pass_over_ipc() {
    let (_l, client, server) = pair(IpcAccess::OwnerOnly).await;
    assert_eq!(peer_kind(server.peer()), PeerKind::Ipc);
    client
        .send(Frame::json(&AdminRequest::PolicyReload).unwrap())
        .await
        .unwrap();
    let frame = server.recv().await.unwrap();
    let got: AdminRequest = decode_for_peer(frame.as_bytes(), peer_kind(server.peer())).unwrap();
    assert_eq!(got, AdminRequest::PolicyReload);
}

#[tokio::test]
async fn second_listener_on_the_same_name_refused() {
    let addr = address();
    let _first = IpcListener::bind(&addr, IpcAccess::OwnerOnly)
        .await
        .unwrap();
    assert!(IpcListener::bind(&addr, IpcAccess::OwnerOnly)
        .await
        .is_err());
}

#[tokio::test]
async fn many_clients_each_accepted() {
    let addr = address();
    let listener = IpcListener::bind(&addr, IpcAccess::OwnerOnly)
        .await
        .unwrap();
    let mut clients = Vec::new();
    for _ in 0..5 {
        clients.push(dialer().dial(&addr, None).await.unwrap());
    }
    for c in &clients {
        let server = listener.accept().await.unwrap();
        exchange(c.as_ref(), server.as_ref()).await;
    }
}

#[cfg(windows)]
async fn raw_client(addr: &str) -> tokio::net::windows::named_pipe::NamedPipeClient {
    tokio::net::windows::named_pipe::ClientOptions::new()
        .open(addr)
        .unwrap()
}

#[cfg(unix)]
async fn raw_client(addr: &str) -> tokio::net::UnixStream {
    tokio::net::UnixStream::connect(addr).await.unwrap()
}

#[tokio::test]
async fn client_without_preamble_not_accepted() {
    use tokio::io::AsyncWriteExt;
    let addr = address();
    let listener = IpcListener::bind(&addr, IpcAccess::OwnerOnly)
        .await
        .unwrap();
    let mut stray = raw_client(&addr).await;
    stray.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
    let mut silent = raw_client(&addr).await;
    let real = dialer().dial(&addr, None).await.unwrap();
    let server = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .expect("the real client is not held up")
        .unwrap();
    exchange(real.as_ref(), server.as_ref()).await;
    let more =
        tokio::time::timeout(PREAMBLE_TIMEOUT + Duration::from_secs(1), listener.accept()).await;
    assert!(
        more.is_err(),
        "neither the stray nor the silent client is accepted"
    );
    let _ = silent.write_all(b"").await;
}

#[tokio::test]
async fn ipc_dial_with_fingerprint_refused_and_missing_endpoint_unreachable() {
    let addr = address();
    assert!(matches!(
        dialer().dial(&addr, Some("sha256:00")).await.err(),
        Some(TransportError::Refused(_))
    ));
    assert!(matches!(
        dialer().dial(&addr, None).await.err(),
        Some(TransportError::Unreachable(_))
    ));
}

#[tokio::test]
async fn ipc_close_is_graceful() {
    let (_l, client, server) = pair(IpcAccess::OwnerOnly).await;
    client
        .send(Frame::from_bytes(b"{\"a\":1}".to_vec()))
        .await
        .unwrap();
    client.close().await;
    assert_eq!(server.recv().await.unwrap().as_bytes(), b"{\"a\":1}");
    assert_eq!(server.recv().await.err(), Some(TransportError::Closed));
}

#[cfg(unix)]
#[tokio::test]
async fn unix_socket_modes_follow_access() {
    use std::os::unix::fs::PermissionsExt;
    for (access, mode) in [
        (IpcAccess::OwnerOnly, 0o600),
        (IpcAccess::OwnerAndAdmins, 0o600),
        (
            IpcAccess::Principals(vec![current_os_user().unwrap()]),
            0o660,
        ),
    ] {
        let addr = address();
        let _l = IpcListener::bind(&addr, access).await.unwrap();
        let got = std::fs::metadata(&addr).unwrap().permissions().mode() & 0o777;
        assert_eq!(got, mode);
    }
}
