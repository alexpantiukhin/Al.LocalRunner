mod common;

use common::*;
use lr_net::{
    read_private, write_private, BrokerIdentity, ListenAddr, NetError, CERT_FILE, DEFAULT_PORT,
    KEY_FILE,
};
use std::net::SocketAddr;

#[test]
fn wildcard_listen_address_refused() {
    for value in ["0.0.0.0:7443", "[::]:7443", "0.0.0.0", "::"] {
        assert!(
            matches!(
                ListenAddr::parse(value, false),
                Err(NetError::BadListenAddress(_))
            ),
            "{value}"
        );
        assert!(
            ListenAddr::parse(value, true).is_err(),
            "{value} in a container"
        );
    }
}

#[test]
fn container_value_only_inside_a_container() {
    assert!(matches!(
        ListenAddr::parse("container", false),
        Err(NetError::BadListenAddress(_))
    ));
    let inside = ListenAddr::parse("container", true).unwrap();
    assert!(inside.socket_addr().ip().is_unspecified());
    assert_eq!(inside.socket_addr().port(), DEFAULT_PORT);
}

#[test]
fn explicit_addresses_accepted_with_default_port() {
    let a = ListenAddr::parse("192.168.1.10:7443", false).unwrap();
    assert_eq!(
        a.socket_addr(),
        "192.168.1.10:7443".parse::<SocketAddr>().unwrap()
    );
    let b = ListenAddr::parse("172.17.0.1", false).unwrap();
    assert_eq!(b.socket_addr().port(), DEFAULT_PORT);
    let c = ListenAddr::parse("[fe80::1]:9000", false).unwrap();
    assert_eq!(c.socket_addr().port(), 9000);
    for bad in ["", "broker.local:7443", "localhost", "1.2.3.4:99999"] {
        assert!(ListenAddr::parse(bad, false).is_err(), "{bad}");
    }
    assert!(ListenAddr::explicit("0.0.0.0:1".parse().unwrap()).is_err());
    assert!(
        ListenAddr::explicit("[::ffff:0.0.0.0]:1".parse().unwrap()).is_err(),
        "a mapped wildcard is a wildcard"
    );
    assert!(ListenAddr::parse("[::ffff:0.0.0.0]:7443", false).is_err());
    assert!(ListenAddr::explicit("[::ffff:127.0.0.1]:1".parse().unwrap()).is_ok());
}

#[test]
fn broker_certificate_survives_restart_and_move() {
    let dir = temp_dir();
    let first = BrokerIdentity::load_or_create(dir.path()).unwrap();
    let again = BrokerIdentity::load_or_create(dir.path()).unwrap();
    assert_eq!(first.fingerprint(), again.fingerprint());
    let moved = temp_dir();
    for f in [CERT_FILE, KEY_FILE] {
        let bytes = std::fs::read(dir.path().join(f)).unwrap();
        write_private(&moved.path().join(f), &bytes).unwrap();
    }
    let elsewhere = BrokerIdentity::load_or_create(moved.path()).unwrap();
    assert_eq!(elsewhere.fingerprint(), first.fingerprint());
    let other = BrokerIdentity::load_or_create(temp_dir().path()).unwrap();
    assert_ne!(other.fingerprint(), first.fingerprint());
}

#[test]
fn identity_debug_hides_the_key() {
    let dir = temp_dir();
    let id = BrokerIdentity::load_or_create(dir.path()).unwrap();
    let key_pem = std::fs::read_to_string(dir.path().join(KEY_FILE)).unwrap();
    let body = key_pem.lines().nth(1).unwrap();
    let shown = format!("{id:?}");
    assert!(!shown.contains(body));
    assert!(shown.contains(&id.fingerprint().to_string()));
}

#[test]
fn missing_key_with_certificate_present_refused() {
    let dir = temp_dir();
    BrokerIdentity::load_or_create(dir.path()).unwrap();
    std::fs::remove_file(dir.path().join(KEY_FILE)).unwrap();
    assert!(BrokerIdentity::load_or_create(dir.path()).is_err());
}

#[test]
fn key_alone_is_a_crashed_first_start_and_is_replaced() {
    let dir = temp_dir();
    let first = BrokerIdentity::load_or_create(dir.path()).unwrap();
    std::fs::remove_file(dir.path().join(CERT_FILE)).unwrap();
    let again = BrokerIdentity::load_or_create(dir.path()).unwrap();
    assert_ne!(again.fingerprint(), first.fingerprint());
    assert!(dir.path().join(CERT_FILE).exists());
    let third = BrokerIdentity::load_or_create(dir.path()).unwrap();
    assert_eq!(third.fingerprint(), again.fingerprint());
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n != CERT_FILE && n != KEY_FILE)
        .collect();
    assert!(
        leftovers.is_empty(),
        "no temporary files left: {leftovers:?}"
    );
}

#[test]
fn private_file_round_trip_replaces_content() {
    let dir = temp_dir();
    let path = dir.path().join("nested").join("secret");
    write_private(&path, b"one").unwrap();
    write_private(&path, b"two").unwrap();
    assert_eq!(read_private(&path).unwrap(), b"two");
}

#[cfg(unix)]
#[test]
fn secret_files_are_owner_only_on_unix() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir();
    BrokerIdentity::load_or_create(dir.path()).unwrap();
    let mode = std::fs::metadata(dir.path().join(KEY_FILE))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let key = lr_net::ClientKey::generate().unwrap();
    let path = dir.path().join("runner.key");
    key.save(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    assert!(matches!(
        lr_net::ClientKey::load(&path),
        Err(NetError::InsecureFile(_))
    ));
}

#[cfg(windows)]
#[test]
fn secret_files_grant_only_the_owner_on_windows() {
    let dir = temp_dir();
    let path = dir.path().join("runner.key");
    lr_net::ClientKey::generate().unwrap().save(&path).unwrap();
    let out = std::process::Command::new("icacls")
        .arg(&path)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let user = lr_net::current_os_user().unwrap().to_lowercase();
    let aces: Vec<&str> = text.lines().filter(|l| l.contains(":(")).collect();
    assert_eq!(aces.len(), 1, "exactly one entry: {text}");
    assert!(aces[0].to_lowercase().contains(&user), "{text}");
    assert!(!aces[0].contains("(I)"), "nothing inherited: {text}");
}

#[cfg(windows)]
#[test]
fn key_file_readable_by_others_refused_on_windows() {
    let dir = temp_dir();
    let path = dir.path().join("runner.key");
    lr_net::ClientKey::generate().unwrap().save(&path).unwrap();
    lr_net::ClientKey::load(&path).expect("owner-only file loads");
    let granted = std::process::Command::new("icacls")
        .arg(&path)
        .args(["/grant", "*S-1-1-0:R"])
        .output()
        .unwrap();
    assert!(granted.status.success());
    assert!(matches!(
        lr_net::ClientKey::load(&path),
        Err(NetError::InsecureFile(_))
    ));
    assert!(matches!(
        read_private(&path),
        Err(NetError::InsecureFile(_))
    ));
}

#[cfg(windows)]
#[test]
fn inherited_broad_access_refused_on_windows() {
    let dir = temp_dir();
    let path = dir.path().join("plain.key");
    std::fs::write(&path, b"x").unwrap();
    let reset = std::process::Command::new("icacls")
        .arg(&path)
        .args(["/reset"])
        .output()
        .unwrap();
    assert!(reset.status.success());
    let users = std::process::Command::new("icacls")
        .arg(&path)
        .args(["/grant", "*S-1-5-32-545:R"])
        .output()
        .unwrap();
    assert!(users.status.success());
    assert!(matches!(
        read_private(&path),
        Err(NetError::InsecureFile(_))
    ));
}
