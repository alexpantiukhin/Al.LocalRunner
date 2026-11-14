use lr_proto::*;
use serde_json::{json, Value};
#[test]
fn history_mine_is_optional_for_older_senders() {
    let old = json!({"type": "history", "project": "p", "active_only": false});
    let ReadRequest::History(f) = serde_json::from_value::<ReadRequest>(old).unwrap() else {
        panic!()
    };
    assert!(!f.mine);
    let v = serde_json::to_value(ReadRequest::History(HistoryFilter::default())).unwrap();
    assert!(v.get("mine").is_none(), "{v}");
    let v = serde_json::to_value(ReadRequest::History(HistoryFilter {
        mine: true,
        ..HistoryFilter::default()
    }))
    .unwrap();
    assert_eq!(v["mine"], true);
}

#[test]
fn unfollow_is_unknown_to_an_older_broker_and_new_cli_messages_to_an_older_hub() {
    let v = serde_json::to_value(ReadRequest::Unfollow { req: 7 }).unwrap();
    assert_eq!(v, json!({"type": "unfollow", "req": 7}));
    let older: Value = json!({"type": "unfollow_all"});
    assert_eq!(
        serde_json::from_value::<ReadRequest>(older).unwrap(),
        ReadRequest::Unknown
    );
    let shutdown = encode(&CliRequest::Shutdown).unwrap();
    assert_eq!(
        decode_for_peer::<CliRequest>(&shutdown, PeerKind::Ipc).unwrap(),
        CliRequest::Shutdown
    );
    assert!(decode_for_peer::<CliRequest>(&shutdown, PeerKind::Network).is_err());
    let err = decode::<HubToBroker>(br#"{"type":"shutdown"}"#).unwrap_err();
    assert_eq!(err, DecodeError::NotOverNetwork("shutdown".into()));
}

#[test]
fn policy_reply_rules_are_optional_for_older_brokers_and_round_trip() {
    let old = json!({"type": "policy", "path": "p.toml", "warnings": []});
    let AdminReply::Policy { rules, .. } = serde_json::from_value::<AdminReply>(old).unwrap()
    else {
        panic!()
    };
    assert!(rules.is_empty());
    let none = AdminReply::Policy {
        path: "p.toml".into(),
        warnings: Vec::new(),
        rules: Vec::new(),
    };
    assert!(serde_json::to_value(&none).unwrap().get("rules").is_none());
    let some = AdminReply::Policy {
        path: "p.toml".into(),
        warnings: Vec::new(),
        rules: vec!["rule 1: when everyone".into()],
    };
    let back: AdminReply = serde_json::from_value(serde_json::to_value(&some).unwrap()).unwrap();
    assert_eq!(back, some);
}
