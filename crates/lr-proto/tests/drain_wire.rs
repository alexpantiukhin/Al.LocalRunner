use lr_proto::*;
use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Old {
    Drain,
}

#[test]
fn drain_without_reason_is_the_old_wire_form() {
    let json = serde_json::to_string(&RunnerToBroker::Drain { reason: None }).unwrap();
    assert_eq!(json, r#"{"type":"drain"}"#);
}

#[test]
fn drain_with_reason_round_trips() {
    let m = RunnerToBroker::Drain {
        reason: Some(DrainReason::Upgrade),
    };
    let json = serde_json::to_string(&m).unwrap();
    assert_eq!(json, r#"{"type":"drain","reason":"upgrade"}"#);
    assert_eq!(serde_json::from_str::<RunnerToBroker>(&json).unwrap(), m);
}

#[test]
fn an_old_enum_reads_a_drain_with_a_reason() {
    let old: Old = serde_json::from_str(r#"{"type":"drain","reason":"upgrade"}"#).unwrap();
    assert_eq!(old, Old::Drain);
}

#[test]
fn a_plain_drain_reads_as_no_reason() {
    let m: RunnerToBroker = serde_json::from_str(r#"{"type":"drain"}"#).unwrap();
    assert_eq!(m, RunnerToBroker::Drain { reason: None });
}
