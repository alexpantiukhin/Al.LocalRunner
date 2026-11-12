use lr_proto::*;
use std::collections::BTreeSet;
use std::str::FromStr;

const SPEC_15: [&str; 13] = [
    "runner_stopped",
    "runner_removed",
    "broker_unavailable",
    "runner_lost",
    "client_removed",
    "sender_gone",
    "hub_absent",
    "service_failed",
    "secret_missing",
    "runner_restarted",
    "cancelled_by_user",
    "disk_insufficient",
    "resource_exceeded",
];

#[test]
fn reason_codes_match_spec_15_list() {
    let all = ReasonCode::all();
    let plain: BTreeSet<&str> = all
        .iter()
        .filter(|c| !c.is_rejection())
        .map(|c| c.as_str())
        .collect();
    assert_eq!(plain, SPEC_15.into_iter().collect::<BTreeSet<_>>());
    let rejected: Vec<&str> = all
        .iter()
        .filter(|c| c.is_rejection())
        .map(|c| c.as_str())
        .collect();
    assert!(!rejected.is_empty());
    assert!(rejected.iter().all(|s| s.starts_with("rejected_")));
    let unique: BTreeSet<&str> = all.iter().map(|c| c.as_str()).collect();
    assert_eq!(unique.len(), all.len(), "codes are unique");
}

#[test]
fn every_code_round_trips_as_its_string() {
    for code in ReasonCode::all() {
        let json = serde_json::to_string(&code).unwrap();
        assert_eq!(json, format!("\"{}\"", code.as_str()));
        assert_eq!(serde_json::from_str::<ReasonCode>(&json).unwrap(), code);
        assert_eq!(ReasonCode::from_str(code.as_str()).unwrap(), code);
        assert_eq!(code.to_string(), code.as_str());
    }
}

#[test]
fn unknown_code_is_preserved() {
    let plain: ReasonCode = serde_json::from_str("\"from_the_future\"").unwrap();
    assert_eq!(plain, ReasonCode::Other("from_the_future".into()));
    assert!(!plain.is_rejection());
    assert_eq!(
        serde_json::to_string(&plain).unwrap(),
        "\"from_the_future\""
    );
    let rejected: ReasonCode = serde_json::from_str("\"rejected_nonsense\"").unwrap();
    assert_eq!(
        rejected,
        ReasonCode::Rejected(RejectReason::Other("rejected_nonsense".into()))
    );
    assert!(rejected.is_rejection());
    assert_eq!(rejected.as_str(), "rejected_nonsense");
    assert!(ReasonCode::from_str("").is_err());
    assert!(ReasonCode::from_str("rejected_").is_err());
    assert!(
        !ReasonCode::all().contains(&plain),
        "all() lists known codes only"
    );
}

#[test]
fn unknown_error_code_is_not_a_command() {
    let code: ProtocolErrorCode = serde_json::from_str("\"brand_new\"").unwrap();
    assert_eq!(code, ProtocolErrorCode::Other);
    assert!(!code.kills_running_jobs());
}

#[test]
fn rejections_are_identified() {
    assert!(ReasonCode::Rejected(RejectReason::Timeout).is_rejection());
    assert!(!ReasonCode::SecretMissing.is_rejection());
}

#[test]
fn intake_refusals_have_their_own_rejected_codes() {
    let all = ReasonCode::all();
    let names: BTreeSet<&str> = all.iter().map(|c| c.as_str()).collect();
    for needed in [
        "rejected_pool_memory",
        "rejected_pool_cpu",
        "rejected_timeout",
        "rejected_reserved",
        "rejected_quota",
        "rejected_no_matching_runner",
        "rejected_no_runner_on_machine",
        "rejected_secret_not_allowed",
        "rejected_zone_not_served",
        "rejected_no_local_broker",
        "rejected_local_user_unknown",
        "rejected_brokers_unreachable",
        "rejected_outbox_expired",
        "rejected_queue_limit",
        "rejected_rate_limited",
        "rejected_commit_not_pushed",
        "rejected_service_not_allowed",
        "rejected_command_not_allowed",
        "rejected_override_above_pool",
    ] {
        assert!(names.contains(needed), "{needed}");
    }
}

#[test]
fn authorization_failures_are_commands_for_the_runner() {
    assert!(ProtocolErrorCode::Unauthorized.kills_running_jobs());
    assert!(ProtocolErrorCode::KeyRevoked.kills_running_jobs());
    assert!(!ProtocolErrorCode::SeqGap.kills_running_jobs());
}
