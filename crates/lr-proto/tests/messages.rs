mod common;

use common::*;
use lr_proto::*;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fmt::Debug;

fn tag_of<T: Message>(msg: &T) -> String {
    let bytes = encode(msg).unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    v["type"].as_str().expect("`type` tag").to_string()
}

fn round_trip_all<T: Message + PartialEq + Debug>(samples: Vec<T>) {
    let mut seen = BTreeSet::new();
    for msg in samples {
        let tag = tag_of(&msg);
        assert!(T::TYPES.contains(&tag.as_str()), "{tag} missing in TYPES");
        let back: T = decode(&encode(&msg).unwrap()).unwrap();
        assert_eq!(back, msg, "{tag}");
        seen.insert(tag);
    }
    let declared: BTreeSet<String> = T::TYPES.iter().map(|s| s.to_string()).collect();
    assert_eq!(seen, declared, "samples cover every declared type");
}

#[test]
fn every_message_round_trips() {
    round_trip_all(runner_to_broker());
    round_trip_all(broker_to_runner());
    round_trip_all(hub_to_broker());
    round_trip_all(broker_to_hub());
    round_trip_all(cli_requests());
    round_trip_all(cli_replies());
    round_trip_all(admin_requests());
    round_trip_all(admin_replies());
    round_trip_all(controls());
    round_trip_all(control_replies());
}

#[test]
fn read_api_and_events_round_trip() {
    for r in read_requests() {
        let call = HubToBroker::Read(ReadCall {
            req: 1,
            request: r.clone(),
        });
        assert_eq!(
            decode::<HubToBroker>(&encode(&call).unwrap()).unwrap(),
            call
        );
    }
    for reply in read_replies() {
        let msg = BrokerToHub::ReadReply(ReadAnswer { req: 1, reply });
        assert_eq!(decode::<BrokerToHub>(&encode(&msg).unwrap()).unwrap(), msg);
    }
    for event in events() {
        let msg = BrokerToHub::Event(EventMsg { sub: 3, event });
        assert_eq!(decode::<BrokerToHub>(&encode(&msg).unwrap()).unwrap(), msg);
    }
}

#[test]
fn read_requests_cover_status_history_explain_log_subscription() {
    let tags: BTreeSet<String> = read_requests()
        .iter()
        .map(|r| {
            serde_json::to_value(r).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    for needed in [
        "status",
        "history",
        "history_show",
        "explain",
        "log",
        "stats",
        "runners",
        "subscribe",
        "unsubscribe",
    ] {
        assert!(tags.contains(needed), "{needed}");
    }
}

#[test]
fn messages_are_tagged_by_type_in_snake_case() {
    assert_eq!(
        serde_json::to_value(RunnerToBroker::Heartbeat { load: None }).unwrap(),
        json!({"type": "heartbeat"})
    );
    assert_eq!(
        tag_of(&RunnerToBroker::ServicesReport(ServicesReport {
            services: vec![]
        })),
        "services_report"
    );
    assert_eq!(
        tag_of(&HubToBroker::CapabilitiesQuery(CapabilitiesQuery {
            req: 1
        })),
        "capabilities_query"
    );
    assert_eq!(
        tag_of(&BrokerToRunner::StopService(StopService {
            name: "s".into()
        })),
        "stop_service"
    );
}

#[test]
fn spec_3_9_message_kinds_all_exist_on_their_channel() {
    let runner: BTreeSet<&str> = RunnerToBroker::TYPES.iter().copied().collect();
    for t in [
        "join",
        "hello",
        "started",
        "chunk",
        "step_result",
        "job_result",
        "heartbeat",
        "resume",
        "drain",
        "bye",
        "services_report",
    ] {
        assert!(runner.contains(t), "runner->broker {t}");
    }
    let to_runner: BTreeSet<&str> = BrokerToRunner::TYPES.iter().copied().collect();
    for t in [
        "assign",
        "cancel",
        "stop_service",
        "resume",
        "heartbeat",
        "bye",
    ] {
        assert!(to_runner.contains(t), "broker->runner {t}");
    }
    let hub: BTreeSet<&str> = HubToBroker::TYPES.iter().copied().collect();
    for t in [
        "join",
        "hello",
        "capabilities_query",
        "order",
        "cancel",
        "resume",
        "heartbeat",
        "bye",
        "read",
    ] {
        assert!(hub.contains(t), "hub->broker {t}");
    }
    let to_hub: BTreeSet<&str> = BrokerToHub::TYPES.iter().copied().collect();
    for t in [
        "capabilities_reply",
        "accepted",
        "rejected",
        "started",
        "chunk",
        "step_result",
        "job_result",
        "cancel_ack",
        "resume",
        "heartbeat",
        "bye",
        "read_reply",
        "event",
    ] {
        assert!(to_hub.contains(t), "broker->hub {t}");
    }
}

#[test]
fn started_carries_no_time_the_broker_uses_its_own_clock() {
    let v = serde_json::to_value(RunnerToBroker::Started(started(1, 1))).unwrap();
    let keys: BTreeSet<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
    assert_eq!(keys, BTreeSet::from(["type", "job", "seq"]));
}

#[test]
fn secrets_travel_as_names_only() {
    let v = serde_json::to_value(HubToBroker::Order(Order {
        job: job(1),
        seq: 1,
        spec: Box::new(order_spec()),
    }))
    .unwrap();
    assert_eq!(
        v["spec"]["requirements"]["secrets"],
        json!(["registry_auth"])
    );
    let v = serde_json::to_value(BrokerToRunner::Assign(Assign {
        job: job(1),
        seq: 1,
        spec: Box::new(assign_spec()),
    }))
    .unwrap();
    assert_eq!(v["spec"]["secrets"], json!(["registry_auth"]));
}

#[test]
fn unknown_message_type_is_reported_as_unknown_and_ignorable() {
    let err = decode::<RunnerToBroker>(br#"{"type":"from_the_future","x":1}"#).unwrap_err();
    assert_eq!(err, DecodeError::UnknownType("from_the_future".into()));
    assert!(err.is_ignorable());
    assert!(err.refusal().is_none());
}

#[test]
fn extra_fields_of_a_newer_minor_are_ignored() {
    let msg: RunnerToBroker =
        decode(br#"{"type":"started","job":"job-1","seq":1,"new_field":true}"#).unwrap();
    assert_eq!(msg, RunnerToBroker::Started(started(1, 1)));
}

#[test]
fn admin_commands_never_exist_on_network_channels() {
    for t in [
        "policy_reload",
        "policy_check",
        "logging_reload",
        "broker_add",
        "runner_remove",
        "client_remove",
        "runner_list",
        "client_list",
    ] {
        let doc = format!(r#"{{"type":"{t}"}}"#);
        for tag_list in [
            RunnerToBroker::TYPES,
            BrokerToRunner::TYPES,
            HubToBroker::TYPES,
            BrokerToHub::TYPES,
        ] {
            assert!(!tag_list.contains(&t), "{t} must not be a network type");
        }
        let err = decode::<HubToBroker>(doc.as_bytes()).unwrap_err();
        assert_eq!(err, DecodeError::NotOverNetwork(t.into()));
        let err = decode::<RunnerToBroker>(doc.as_bytes()).unwrap_err();
        assert_eq!(err, DecodeError::NotOverNetwork(t.into()));
        assert!(!err.is_ignorable());
        let refusal = err.refusal().unwrap();
        assert_eq!(refusal.code, ProtocolErrorCode::AdminOverNetwork);
    }
}

#[test]
fn admin_commands_are_accepted_over_local_ipc_only() {
    for req in admin_requests() {
        let bytes = encode(&req).unwrap();
        assert_eq!(
            decode_for_peer::<AdminRequest>(&bytes, PeerKind::Ipc).unwrap(),
            req
        );
        let err = decode_for_peer::<AdminRequest>(&bytes, PeerKind::Network).unwrap_err();
        assert!(matches!(err, DecodeError::NotOverNetwork(_)), "{err:?}");
        assert_eq!(
            err.refusal().unwrap().code,
            ProtocolErrorCode::AdminOverNetwork
        );
    }
}

#[test]
fn done_is_cli_to_hub_ipc_only() {
    for tags in [
        RunnerToBroker::TYPES,
        BrokerToRunner::TYPES,
        HubToBroker::TYPES,
        BrokerToHub::TYPES,
    ] {
        assert!(!tags.contains(&"done"));
    }
    let bytes = encode(&CliRequest::Done { job: job(1) }).unwrap();
    assert!(decode_for_peer::<CliRequest>(&bytes, PeerKind::Ipc).is_ok());
    assert!(matches!(
        decode_for_peer::<CliRequest>(&bytes, PeerKind::Network).unwrap_err(),
        DecodeError::NotOverNetwork(_)
    ));
}

#[test]
fn runner_control_channel_is_local_only() {
    for c in controls() {
        let bytes = encode(&c).unwrap();
        assert!(decode_for_peer::<RunnerControl>(&bytes, PeerKind::Network).is_err());
        assert_eq!(
            decode_for_peer::<RunnerControl>(&bytes, PeerKind::Ipc).unwrap(),
            c
        );
    }
}

#[test]
fn network_channels_decode_on_both_peer_kinds() {
    let bytes = encode(&RunnerToBroker::Heartbeat { load: None }).unwrap();
    assert!(decode_for_peer::<RunnerToBroker>(&bytes, PeerKind::Network).is_ok());
    assert!(decode_for_peer::<RunnerToBroker>(&bytes, PeerKind::Ipc).is_ok());
}

#[test]
fn malformed_documents_are_refused_with_a_protocol_error() {
    for doc in [
        &b"not json"[..],
        br#"{"no_type":1}"#,
        br#"[1,2]"#,
        br#"{"type":7}"#,
        br#"{"type":"started","job":1}"#,
    ] {
        let err = decode::<RunnerToBroker>(doc).unwrap_err();
        assert!(matches!(err, DecodeError::Malformed(_)), "{err:?}");
        assert!(!err.is_ignorable());
        assert_eq!(err.refusal().unwrap().code, ProtocolErrorCode::Malformed);
    }
}

#[test]
fn oversized_frame_maps_to_its_own_error_code() {
    let err = DecodeError::TooLarge { len: 9 };
    assert_eq!(
        err.refusal().unwrap().code,
        ProtocolErrorCode::FrameTooLarge
    );
}

#[test]
fn final_job_result_must_carry_a_final_state() {
    let mut r = job_result(1);
    r.state = lr_core::JobState::Running;
    let err = decode::<RunnerToBroker>(&encode(&RunnerToBroker::JobResult(r.clone())).unwrap())
        .unwrap_err();
    assert!(matches!(err, DecodeError::Malformed(_)));
    let err = decode::<BrokerToHub>(&encode(&BrokerToHub::JobResult(r)).unwrap()).unwrap_err();
    assert!(matches!(err, DecodeError::Malformed(_)));
}

#[test]
fn rejection_must_carry_a_rejected_code() {
    let bad = BrokerToHub::Rejected(Rejected {
        job: job(1),
        order_seq: 1,
        code: ReasonCode::RunnerLost,
        detail: "x".into(),
    });
    assert!(matches!(
        decode::<BrokerToHub>(&encode(&bad).unwrap()).unwrap_err(),
        DecodeError::Malformed(_)
    ));
}

#[test]
fn seq_zero_is_malformed_because_counters_start_at_one() {
    let msgs = [
        encode(&RunnerToBroker::Chunk(chunk(0))).unwrap(),
        encode(&RunnerToBroker::Started(started(1, 0))).unwrap(),
    ];
    for bytes in msgs {
        assert!(matches!(
            decode::<RunnerToBroker>(&bytes).unwrap_err(),
            DecodeError::Malformed(_)
        ));
    }
    let order = HubToBroker::Order(Order {
        job: job(1),
        seq: 0,
        spec: Box::new(order_spec()),
    });
    assert!(decode::<HubToBroker>(&encode(&order).unwrap()).is_err());
}

#[test]
fn join_carries_code_key_and_capabilities() {
    let v = serde_json::to_value(RunnerToBroker::Join(join(Role::Runner))).unwrap();
    assert_eq!(v["code"], "code-123");
    assert_eq!(v["public_key"], "pk");
    assert_eq!(v["role"], "runner");
    assert!(v["capabilities"]["platforms"].is_array());
    let v = serde_json::to_value(HubToBroker::Join(join(Role::Hub))).unwrap();
    assert_eq!(v["role"], "hub");
}

#[test]
fn welcome_tells_hub_its_zone_and_runner_the_kill_deadline() {
    let v = serde_json::to_value(BrokerToHub::Welcome(welcome())).unwrap();
    assert_eq!(v["zone"], "lan");
    assert_eq!(v["heartbeat_s"], 15);
    let v = serde_json::to_value(BrokerToRunner::Welcome(welcome())).unwrap();
    assert_eq!(v["broker_lost_kill_s"], 180);
}

#[test]
fn join_code_errors_have_distinct_codes() {
    let codes: BTreeSet<String> = [
        ProtocolErrorCode::JoinCodeUnknown,
        ProtocolErrorCode::JoinCodeExpired,
        ProtocolErrorCode::JoinCodeUsedUp,
        ProtocolErrorCode::JoinCodeKindMismatch,
        ProtocolErrorCode::JoinCodeNameMismatch,
        ProtocolErrorCode::MachineIdClash,
    ]
    .iter()
    .map(|c| serde_json::to_string(c).unwrap())
    .collect();
    assert_eq!(codes.len(), 6);
}

#[test]
fn resume_reports_last_seq_per_job() {
    let v = serde_json::to_value(RunnerToBroker::Resume(resume())).unwrap();
    assert_eq!(v["points"][0], json!({"job": "job-1", "last_seq": 7}));
    assert_eq!(v["points"][1], json!({"job": "job-2", "last_seq": 0}));
}

#[test]
fn services_report_names_state_memory_and_remaining_life() {
    let v = serde_json::to_value(service_report()).unwrap();
    assert_eq!(
        v,
        json!({"name": "build-daemon", "state": "idle", "mem_mb": 2048, "remaining_s": 900})
    );
    let states: BTreeSet<String> = [
        ServiceState::Starting,
        ServiceState::Idle,
        ServiceState::Busy,
        ServiceState::Stopping,
        ServiceState::Stopped,
        ServiceState::Failed,
        ServiceState::Unavailable,
    ]
    .iter()
    .map(|s| {
        serde_json::to_value(s)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    })
    .collect();
    assert_eq!(states.len(), 7);
}

#[test]
fn accepted_and_rejected_answer_a_specific_order_seq() {
    let v = serde_json::to_value(BrokerToHub::Accepted(Accepted {
        job: job(1),
        order_seq: 4,
        waiting: None,
        timeout_trimmed_to: None,
    }))
    .unwrap();
    assert_eq!(v["order_seq"], 4);
    assert!(v.get("timeout_trimmed_to").is_none());
    let older: BrokerToHub = serde_json::from_value(v).unwrap();
    let BrokerToHub::Accepted(a) = older else {
        panic!("not accepted")
    };
    assert_eq!(a.timeout_trimmed_to, None);
}

#[test]
fn heartbeat_has_no_payload_and_drain_is_a_runner_message() {
    assert!(RunnerToBroker::TYPES.contains(&"drain"));
    assert!(!HubToBroker::TYPES.contains(&"drain"));
}
