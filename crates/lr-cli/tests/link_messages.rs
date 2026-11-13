use lr_cli::commands::timefmt::format_time;
use lr_cli::commands::view::unreachable_line;
use lr_core::Timestamp;
use lr_proto::BrokerStatus;

#[test]
fn unreachable_line_names_the_broker_and_the_time() {
    let t = Timestamp(1_700_000_000_000);
    let line = unreachable_line("local", Some(t));
    assert_eq!(
        line,
        format!(
            "broker local unreachable since {} (the hub retries on its own)",
            format_time(t)
        )
    );
    assert!(!line.contains("restart the hub"), "{line}");
    assert!(!line.contains("down hub"), "{line}");
}

#[test]
fn unreachable_line_without_a_time_still_names_the_broker() {
    assert_eq!(
        unreachable_line("local", None),
        "broker local unreachable (the hub retries on its own)"
    );
}

#[test]
fn old_hub_reply_without_since_parses() {
    let json = r#"{"name":"local","zone":"local","available":false,"outbox":0}"#;
    let b: BrokerStatus = serde_json::from_str(json).unwrap();
    assert_eq!(b.since, None);
}
