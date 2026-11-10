mod common;

use common::*;
use lr_abstractions::{Dialer, Listener, Peer, PeerAccess, TransportError};
use lr_core::{Endpoint, Site, Zone};
use lr_net::{
    current_os_principal, current_os_user, IpcAccess, IpcDialer, IpcListener, LocalUser,
    LocalUsers, ServerIdentity,
};
use lr_proto::RejectReason;
use std::time::Duration;

fn address() -> String {
    let name = unique("lr-trust");
    if cfg!(windows) {
        format!(r"\\.\pipe\{name}")
    } else {
        std::env::temp_dir()
            .join(format!("{name}.sock"))
            .to_string_lossy()
            .into_owned()
    }
}

fn stranger() -> &'static str {
    if cfg!(windows) {
        "S-1-5-21-1-2-3-4242"
    } else {
        "4242424"
    }
}

#[tokio::test]
async fn server_of_another_identity_refused_before_anything_is_sent() {
    let addr = address();
    let listener = IpcListener::bind(&addr, IpcAccess::OwnerOnly)
        .await
        .unwrap();
    let err = IpcDialer::new(ServerIdentity::principal(stranger()))
        .dial(&addr, None)
        .await
        .err()
        .expect("a server run by someone else is refused");
    match &err {
        TransportError::Refused(why) => assert!(why.starts_with("untrusted_server"), "{why}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let accepted = tokio::time::timeout(Duration::from_secs(1), listener.accept()).await;
    assert!(accepted.is_err(), "not even the preamble was sent");

    IpcDialer::new(ServerIdentity::principal(stranger()).or(current_os_principal().unwrap()))
        .dial(&addr, None)
        .await
        .expect("the expected owner is accepted");
    tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn server_identity_lists_whom_it_accepts() {
    let id = ServerIdentity::principal("S-1-5-21-9-9-9-1001").or("S-1-5-21-9-9-9-1002");
    assert!(id.accepts("S-1-5-21-9-9-9-1001"));
    assert!(id.accepts("s-1-5-21-9-9-9-1002"));
    assert!(!id.accepts("S-1-5-21-9-9-9-1003"));
    assert!(!id.accepts(""));
    let admins = ServerIdentity::principal("S-1-5-21-9-9-9-1001").or_admins();
    if cfg!(windows) {
        assert!(admins.accepts("S-1-5-18"));
        assert!(admins.accepts("S-1-5-32-544"));
    } else {
        assert!(admins.accepts("0"));
    }
    assert!(!ServerIdentity::principal("S-1-5-21-9-9-9-1001").accepts("S-1-5-18"));
}

#[tokio::test]
async fn principals_access_admits_listed_users_and_reloads() {
    let me = current_os_user().unwrap();
    let addr = address();
    let listener = IpcListener::bind(&addr, IpcAccess::Principals(vec![me.clone()]))
        .await
        .unwrap();
    let dialer = IpcDialer::new(ServerIdentity::current_user().unwrap());
    dialer
        .dial(&addr, None)
        .await
        .expect("listed user connects");
    let first = listener.accept().await.unwrap();
    assert_eq!(first.peer().access, PeerAccess::Local);
    listener
        .set_access(IpcAccess::Principals(vec![me.clone()]))
        .await
        .unwrap();
    dialer.dial(&addr, None).await.expect("still after reload");
    listener.accept().await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn unknown_account_in_the_list_refused() {
    let bogus = r"NO-SUCH-DOMAIN-42\nobody-at-all".to_string();
    assert!(
        IpcListener::bind(&address(), IpcAccess::Principals(vec![bogus.clone()]))
            .await
            .is_err()
    );
    let listener = IpcListener::bind(&address(), IpcAccess::OwnerOnly)
        .await
        .unwrap();
    assert!(listener
        .set_access(IpcAccess::Principals(vec![bogus]))
        .await
        .is_err());
}

#[cfg(windows)]
#[test]
fn windows_os_users_are_qualified() {
    let me = current_os_user().unwrap();
    assert!(me.contains('\\'), "DOMAIN\\name expected: {me}");
}

fn ipc(os_user: &str) -> Peer {
    Peer {
        endpoint: Endpoint::Ipc {
            os_user: os_user.into(),
        },
        key: None,
        access: PeerAccess::Local,
    }
}

fn entry(name: &str) -> LocalUser {
    LocalUser {
        name: name.into(),
        label: name.into(),
    }
}

#[cfg(windows)]
#[test]
fn same_short_name_in_another_domain_is_another_user() {
    let bare = LocalUsers::with_machine(vec![entry("alex")], "PC");
    assert!(bare.admit(&ipc(r"PC\alex")).is_ok(), "a bare name is local");
    assert!(bare.admit(&ipc(r"pc\ALEX")).is_ok(), "names ignore case");
    assert_eq!(
        bare.admit(&ipc(r"CORP\alex")),
        Err(RejectReason::LocalUserUnknown),
        "a bare name never matches a domain account"
    );
    assert_eq!(
        bare.admit(&ipc("alex")),
        Err(RejectReason::LocalUserUnknown)
    );
    let domain = LocalUsers::with_machine(vec![entry(r"CORP\alex")], "PC");
    assert!(domain.admit(&ipc(r"CORP\alex")).is_ok());
    assert_eq!(
        domain.admit(&ipc(r"PC\alex")),
        Err(RejectReason::LocalUserUnknown)
    );
    let both = LocalUsers::with_machine(vec![entry(r"CORP\alex"), entry("alex")], "PC");
    let corp = both.admit(&ipc(r"CORP\alex")).unwrap();
    let local = both.admit(&ipc(r"PC\alex")).unwrap();
    assert_ne!(corp.client_key, local.client_key);
    assert_eq!(corp.client_key, r"local:CORP\alex");
    assert_eq!(local.client_key, r"local:PC\alex");
    assert_eq!(corp.label, r"CORP\alex");
    assert_eq!(local.label, "alex");
}

#[test]
fn zone_compares_qualified_users() {
    let site = Site {
        broker_addrs: vec!["192.168.1.10".parse().unwrap()],
        networks: Default::default(),
    };
    let corp = ipc(r"CORP\alex").endpoint;
    let local = ipc(r"PC\alex").endpoint;
    assert_eq!(site.zone_between(&corp, &corp), Zone::Local);
    assert_ne!(site.zone_between(&corp, &local), Zone::Local);
}

#[cfg(windows)]
#[tokio::test]
async fn sid_entry_matches_the_account_it_names() {
    let sid = current_os_principal().unwrap();
    let (users, unknown, lower) = tokio::task::spawn_blocking(move || {
        (
            LocalUsers::new(vec![entry(&sid)]),
            LocalUsers::new(vec![entry("S-1-5-21-1-2-3-4242")]),
            LocalUsers::new(vec![entry(&sid.to_lowercase())]),
        )
    })
    .await
    .unwrap();
    let addr = address();
    let listener = IpcListener::bind(&addr, IpcAccess::OwnerOnly)
        .await
        .unwrap();
    IpcDialer::new(ServerIdentity::current_user().unwrap())
        .dial(&addr, None)
        .await
        .unwrap();
    let server = listener.accept().await.unwrap();
    let me = users
        .admit(server.peer())
        .expect("the SID of the connecting user");
    assert_eq!(me.os_user, current_os_user().unwrap());
    assert!(lower.admit(server.peer()).is_ok());
    assert_eq!(
        unknown.admit(server.peer()),
        Err(RejectReason::LocalUserUnknown)
    );
    let raw = LocalUsers::with_machine(vec![entry("S-1-5-21-1-2-3-4242")], "PC");
    assert_eq!(
        raw.admit(&ipc(r"PC\S-1-5-21-1-2-3-4242")),
        Err(RejectReason::LocalUserUnknown),
        "a SID is never taken for a local account name"
    );
}
