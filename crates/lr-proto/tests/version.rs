use lr_proto::*;

#[test]
fn protocol_version_starts_at_one() {
    assert_eq!(PROTOCOL_VERSION, Version { major: 1, minor: 0 });
    assert_eq!(PROTOCOL_VERSION.to_string(), "1.0");
}

#[test]
fn same_version_is_accepted() {
    assert_eq!(negotiate(PROTOCOL_VERSION).unwrap(), PROTOCOL_VERSION);
}

#[test]
fn unknown_version_refused() {
    let peer = Version { major: 2, minor: 0 };
    let refused = negotiate(peer).unwrap_err();
    assert_eq!(refused.peer, peer);
    assert_eq!(refused.ours, PROTOCOL_VERSION);
    let text = refused.to_string();
    assert!(text.contains("2.0") && text.contains("1.0"), "{text}");
    assert!(text.contains("unsupported"), "{text}");
}

#[test]
fn older_major_is_refused_too() {
    let ours = Version { major: 3, minor: 0 };
    let theirs = Version { major: 2, minor: 9 };
    assert!(negotiate_between(ours, theirs).is_err());
}

#[test]
fn refusal_is_a_protocol_error_naming_the_supported_version() {
    let err = negotiate(Version { major: 9, minor: 1 })
        .unwrap_err()
        .into_error();
    assert_eq!(err.code, ProtocolErrorCode::UnsupportedVersion);
    assert_eq!(err.supported, Some(PROTOCOL_VERSION));
    assert!(err.message.contains("9.1"));
}

#[test]
fn newer_minor_within_the_major_works_at_the_lower_minor() {
    let ours = Version { major: 1, minor: 2 };
    let theirs = Version { major: 1, minor: 5 };
    assert_eq!(negotiate_between(ours, theirs).unwrap(), ours);
    assert_eq!(negotiate_between(theirs, ours).unwrap(), ours);
}

#[test]
fn heartbeat_every_15_seconds_and_three_misses_mean_offline() {
    assert_eq!(HEARTBEAT_INTERVAL_S, 15);
    assert_eq!(MISSED_HEARTBEATS_OFFLINE, 3);
}
