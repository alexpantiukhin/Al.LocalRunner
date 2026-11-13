use lr_cli::commands::timefmt::format_time;
use lr_cli::commands::view::unreachable_line;
use lr_cli::link_report::{not_connected_line, runner_link_line};
use lr_core::Timestamp;
use lr_proto::{BrokerStatus, ControlReply};

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

#[test]
fn not_connected_line_names_the_process_and_the_time() {
    let t = Timestamp(1_700_000_000_000);
    assert_eq!(
        not_connected_line("runner r1", Some(t)),
        format!(
            "runner r1: alive but not connected to the broker since {}",
            format_time(t)
        )
    );
}

#[test]
fn not_connected_line_without_time() {
    assert_eq!(
        not_connected_line("hub (broker local)", None),
        "hub (broker local): alive but not connected to the broker"
    );
}

#[test]
fn a_runner_that_answers_failed_to_status_gives_no_line() {
    let failed = ControlReply::Failed {
        message: "unknown control command `status`".into(),
    };
    assert_eq!(runner_link_line("r1", &failed), None);
    assert_eq!(runner_link_line("r1", &ControlReply::Ok), None);
    let connected = ControlReply::Link {
        connected: true,
        since: None,
    };
    assert_eq!(runner_link_line("r1", &connected), None);
    let lost = ControlReply::Link {
        connected: false,
        since: None,
    };
    assert_eq!(
        runner_link_line("r1", &lost).as_deref(),
        Some("runner r1: alive but not connected to the broker")
    );
}
