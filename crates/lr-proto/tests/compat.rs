mod common;

use common::*;
use lr_core::{Zone, MAX_FRAME_BYTES};
use lr_proto::*;
use serde_json::json;

#[test]
fn admin_types_have_a_single_source() {
    assert_eq!(ADMIN_TYPES, <AdminRequest as Message>::TYPES);
    for t in ADMIN_TYPES {
        let doc = format!(r#"{{"type":"{t}"}}"#);
        assert!(matches!(
            decode::<BrokerToHub>(doc.as_bytes()).unwrap_err(),
            DecodeError::NotOverNetwork(_)
        ));
    }
}

#[test]
fn local_only_channel_checks_raw_size_before_parsing() {
    let bytes = vec![b'{'; MAX_FRAME_BYTES + 1];
    assert!(matches!(
        decode_for_peer::<AdminRequest>(&bytes, PeerKind::Network).unwrap_err(),
        DecodeError::TooLarge { .. }
    ));
}

#[test]
fn ignored_unknown_types_come_back_as_a_value_to_log() {
    let got = receive::<HubToBroker>(br#"{"type":"future_thing"}"#, PeerKind::Network).unwrap();
    assert_eq!(got, Received::Ignored("future_thing".into()));
    let got = receive::<HubToBroker>(br#"{"type":"heartbeat"}"#, PeerKind::Network).unwrap();
    assert_eq!(got, Received::Message(HubToBroker::Heartbeat));
    assert!(receive::<HubToBroker>(b"junk", PeerKind::Network).is_err());
}

#[test]
fn unknown_nested_tags_do_not_fail_the_message() {
    let doc = br#"{"type":"read","req":1,"request":{"type":"flux_capacitor","x":1}}"#;
    let msg: HubToBroker = decode(doc).unwrap();
    assert_eq!(
        msg,
        HubToBroker::Read(ReadCall {
            req: 1,
            request: ReadRequest::Unknown
        })
    );
    let doc = br#"{"type":"read_reply","req":1,"reply":{"type":"hologram"}}"#;
    let msg: BrokerToHub = decode(doc).unwrap();
    assert!(matches!(
        msg,
        BrokerToHub::ReadReply(ReadAnswer {
            reply: ReadReply::Unknown,
            ..
        })
    ));
    let doc = br#"{"type":"event","sub":1,"event":{"type":"meteor"}}"#;
    let msg: BrokerToHub = decode(doc).unwrap();
    assert!(matches!(
        msg,
        BrokerToHub::Event(EventMsg {
            event: QueueEvent::Unknown,
            ..
        })
    ));
}

#[test]
fn unknown_enum_values_do_not_fail_the_message() {
    let doc =
        br#"{"type":"read","req":1,"request":{"type":"subscribe","topics":["queue","weather"]}}"#;
    let HubToBroker::Read(call) = decode::<HubToBroker>(doc).unwrap() else {
        panic!()
    };
    assert_eq!(
        call.request,
        ReadRequest::Subscribe {
            topics: vec![Topic::Queue, Topic::Unknown]
        }
    );
    let doc = br#"{"type":"services_report","services":[{"name":"s","state":"hibernating","mem_mb":1,"remaining_s":null}]}"#;
    let RunnerToBroker::ServicesReport(r) = decode::<RunnerToBroker>(doc).unwrap() else {
        panic!()
    };
    assert_eq!(r.services[0].state, ServiceState::Unknown);
    let doc = br#"{"type":"cancel_ack","job":"j","seq":1,"cancel_seq":1,"outcome":"teleported","denial":null}"#;
    let BrokerToHub::CancelAck(a) = decode::<BrokerToHub>(doc).unwrap() else {
        panic!()
    };
    assert_eq!(a.outcome, CancelOutcome::Unknown);
    let doc = br#"{"type":"error","code":"brand_new","message":"m","supported":null}"#;
    let RunnerToBroker::Error(e) = decode::<RunnerToBroker>(doc).unwrap() else {
        panic!()
    };
    assert_eq!(e.code, ProtocolErrorCode::Other);
}

#[test]
fn unknown_service_status_counts_as_unavailable() {
    let mut v = serde_json::to_value(caps()).unwrap();
    v["services"]["x"] = json!("on_fire");
    let wire: WireCapabilities = serde_json::from_value(v).unwrap();
    assert_eq!(wire.services["x"], WireServiceStatus::Unknown);
    let c = wire.into_capabilities(Zone::Lan);
    assert_eq!(c.services["x"], lr_core::ServiceStatus::Unavailable);
}

#[test]
fn extra_fields_on_nested_enums_and_structs_are_ignored() {
    let doc = br#"{"type":"read_reply","req":1,"reply":{"type":"subscribed","sub":4,"extra":[1]},"more":1}"#;
    let msg: BrokerToHub = decode(doc).unwrap();
    assert_eq!(
        msg,
        BrokerToHub::ReadReply(ReadAnswer {
            req: 1,
            reply: ReadReply::Subscribed { sub: 4 }
        })
    );
    let doc = br#"{"type":"event","sub":1,"event":{"type":"runner_online","name":"r","since":5}}"#;
    assert!(decode::<BrokerToHub>(doc).is_ok());
}

#[test]
fn restarted_stream_starts_again_at_one_for_the_next_broker() {
    let mut stream = SeqStream::new();
    assert_eq!(stream.next_seq(), 1, "the order is seq 1");
    assert_eq!(stream.next_seq(), 2, "the cancel is seq 2");
    assert_eq!(stream.last(), 2);
    stream.restart();
    assert_eq!(stream.last(), 0);
    assert_eq!(stream.next_seq(), 1);
}

#[test]
fn restarted_stream_is_a_new_stream_for_deduplication() {
    use std::collections::HashSet;
    let order = |seq| {
        HubToBroker::Order(Order {
            job: job(1),
            seq,
            spec: Box::new(order_spec()),
        })
    };
    let mut stream = SeqStream::new();
    let mut seen_a: HashSet<DedupKey> = HashSet::new();
    let mut last_a = 0;
    for _ in 0..2 {
        let seq = stream.next_seq();
        assert_eq!(classify_seq(last_a, seq), SeqCheck::Next);
        last_a = seq;
        assert!(seen_a.insert(DedupKey::of("hub", &order(seq)).unwrap()));
    }
    assert_eq!(
        classify_seq(last_a, 2),
        SeqCheck::Repeat,
        "resend to broker A"
    );

    stream.restart();
    let seq = stream.next_seq();
    assert_eq!(seq, 1);
    let key = DedupKey::of("hub", &order(seq)).unwrap();
    let seen_b: HashSet<DedupKey> = HashSet::new();
    assert_eq!(
        classify_seq(0, seq),
        SeqCheck::Next,
        "broker B accepts seq 1"
    );
    assert!(
        !seen_b.contains(&key),
        "a pre-restart seq is no duplicate for B"
    );
    assert!(
        seen_a.contains(&key),
        "while broker A would see it as a repeat"
    );
    assert_eq!(classify_seq(last_a, seq), SeqCheck::Repeat);
}

#[test]
fn worst_case_escaped_log_chunk_fits_a_frame() {
    let l = LogChunk {
        req: u64::MAX,
        text: "\u{0}".repeat(CHUNK_TEXT_LIMIT),
        end: true,
    };
    let bytes = encode(&BrokerToHub::LogChunk(l.clone())).unwrap();
    assert!(bytes.len() <= FRAME_LIMIT, "{}", bytes.len());
    assert_eq!(
        decode::<BrokerToHub>(&bytes).unwrap(),
        BrokerToHub::LogChunk(l)
    );
}

#[test]
fn log_chunk_over_the_text_limit_is_malformed() {
    let l = LogChunk {
        req: 1,
        text: "a".repeat(CHUNK_TEXT_LIMIT + 1),
        end: false,
    };
    assert!(matches!(
        decode::<BrokerToHub>(&encode(&BrokerToHub::LogChunk(l)).unwrap()).unwrap_err(),
        DecodeError::Malformed(_)
    ));
}

#[test]
fn classify_seq_distinguishes_next_repeat_and_gap() {
    assert_eq!(classify_seq(4, 5), SeqCheck::Next);
    assert_eq!(classify_seq(4, 4), SeqCheck::Repeat);
    assert_eq!(classify_seq(4, 1), SeqCheck::Repeat);
    assert_eq!(classify_seq(4, 7), SeqCheck::Gap { expected: 5 });
}

#[test]
fn worst_case_escaped_chunk_fits_a_frame() {
    let c = Chunk {
        job: job(1),
        seq: u64::MAX,
        step: Some(u32::MAX),
        text: "\u{0}".repeat(CHUNK_TEXT_LIMIT),
    };
    let bytes = encode(&RunnerToBroker::Chunk(c.clone())).unwrap();
    assert!(bytes.len() <= FRAME_LIMIT, "{}", bytes.len());
    assert!(
        bytes.len() > CHUNK_TEXT_LIMIT * 5,
        "controls escape to six bytes"
    );
    assert_eq!(
        decode::<RunnerToBroker>(&bytes).unwrap(),
        RunnerToBroker::Chunk(c)
    );
}

#[test]
fn chunk_over_the_text_limit_is_malformed() {
    let c = chunk_of(CHUNK_TEXT_LIMIT + 1);
    assert!(matches!(
        decode::<RunnerToBroker>(&encode(&RunnerToBroker::Chunk(c.clone())).unwrap()).unwrap_err(),
        DecodeError::Malformed(_)
    ));
    assert!(matches!(
        decode::<BrokerToHub>(&encode(&BrokerToHub::Chunk(c)).unwrap()).unwrap_err(),
        DecodeError::Malformed(_)
    ));
}

fn chunk_of(len: usize) -> Chunk {
    Chunk {
        job: job(1),
        seq: 1,
        step: None,
        text: "a".repeat(len),
    }
}

#[test]
fn split_text_cuts_at_character_boundaries() {
    let text = "aé€😀".repeat(5);
    for limit in [1usize, 2, 3, 4, 5, 7, 100] {
        let parts = split_text(&text, limit);
        assert_eq!(parts.concat(), text);
        for p in &parts {
            assert!(p.len() <= limit.max(4), "{limit}: {}", p.len());
        }
    }
    assert!(split_text("", 8).is_empty());
    assert_eq!(split_text("abcdef", 4), ["abcd", "ef"]);
}

#[test]
fn read_pages_are_bounded() {
    let h = HistoryFilter {
        limit: Some(1_000_000),
        ..HistoryFilter::default()
    };
    assert_eq!(h.effective_limit(), HISTORY_LIMIT_MAX);
    assert_eq!(
        HistoryFilter::default().effective_limit(),
        HISTORY_LIMIT_DEFAULT
    );
    let h = HistoryFilter {
        limit: Some(0),
        ..HistoryFilter::default()
    };
    assert_eq!(h.effective_limit(), 1);
    let s = StatusFilter {
        limit: Some(9999),
        ..StatusFilter::default()
    };
    assert_eq!(s.effective_limit(), STATUS_LIMIT_MAX);
    assert_eq!(
        StatusFilter::default().effective_limit(),
        STATUS_LIMIT_DEFAULT
    );
}

#[test]
fn history_and_status_answers_carry_a_continuation_token() {
    let v = serde_json::to_value(&read_replies()[1]).unwrap();
    assert!(v.get("next").is_some());
    let v = serde_json::to_value(&read_replies()[0]).unwrap();
    assert_eq!(v["next"], "q2");
}

#[test]
fn cancel_answer_names_outcome_and_denial() {
    let v = serde_json::to_value(CancelAck {
        job: job(1),
        seq: 1,
        cancel_seq: 2,
        outcome: CancelOutcome::AlreadyFinal,
        denial: None,
    })
    .unwrap();
    assert_eq!(v["outcome"], "already_final");
    assert_eq!(
        serde_json::to_value(CancelOutcome::Applied).unwrap(),
        "applied"
    );
    assert_eq!(
        serde_json::to_value(CancelOutcome::Denied).unwrap(),
        "denied"
    );
}

#[test]
fn hub_cancel_carries_a_reason_code_and_text() {
    let HubToBroker::Cancel(c) = &hub_to_broker()[4] else {
        panic!()
    };
    let v = serde_json::to_value(c).unwrap();
    assert_eq!(v["code"], "sender_gone");
    assert_eq!(v["reason"], "stuck");
}

#[test]
fn join_reply_is_the_only_reply_and_carries_session_facts() {
    let v = serde_json::to_value(BrokerToRunner::Joined(joined())).unwrap();
    assert_eq!(v["zone"], "lan");
    assert_eq!(v["heartbeat_s"], 15);
    assert_eq!(v["broker_lost_kill_s"], 180);
    assert_eq!(
        serde_json::to_value(ProtocolErrorCode::NameTaken).unwrap(),
        "name_taken"
    );
}

#[test]
fn runner_zone_is_its_own() {
    let r = RunnerCaps {
        name: "r".into(),
        online: true,
        zone: Zone::Local,
        caps: caps(),
    };
    assert_eq!(r.into_capabilities().zone, Zone::Local);
    let v = serde_json::to_value(r_with(Zone::Remote)).unwrap();
    assert_eq!(v["zone"], "remote");
}

fn r_with(zone: Zone) -> RunnerCaps {
    RunnerCaps {
        name: "r".into(),
        online: true,
        zone,
        caps: caps(),
    }
}

#[test]
fn grace_change_and_capability_update_reach_the_other_side() {
    assert!(BrokerToRunner::TYPES.contains(&"settings_update"));
    assert!(RunnerToBroker::TYPES.contains(&"capabilities_update"));
    let v = serde_json::to_value(BrokerToRunner::SettingsUpdate(SettingsUpdate {
        broker_lost_kill_s: Some(30),
    }))
    .unwrap();
    assert_eq!(v["broker_lost_kill_s"], 30);
}

#[test]
fn started_notice_names_the_runner_and_the_broker_clock() {
    let v = serde_json::to_value(StartedNotice {
        job: job(1),
        seq: 1,
        runner: "r-1".into(),
        started_at: lr_core::Timestamp(5),
    })
    .unwrap();
    assert_eq!(v["runner"], "r-1");
    assert_eq!(v["started_at"], 5);
}

#[test]
fn accepted_may_say_why_it_waits() {
    let BrokerToHub::Accepted(a) = &broker_to_hub()[3] else {
        panic!()
    };
    assert_eq!(a.waiting.as_deref(), Some("runner busy"));
}

#[test]
fn resume_treats_unlisted_jobs_as_zero() {
    let r = resume();
    assert_eq!(r.last_seq(&job(1)), 7);
    assert_eq!(r.last_seq(&job(2)), 0);
    assert_eq!(r.last_seq(&job(99)), 0);
}

#[test]
fn local_channels_open_with_a_version_and_a_channel() {
    let open = IpcOpen::Hello {
        version: PROTOCOL_VERSION,
        channel: IpcChannel::Admin,
    };
    let bytes = encode(&open).unwrap();
    assert_eq!(
        decode_for_peer::<IpcOpen>(&bytes, PeerKind::Ipc).unwrap(),
        open
    );
    assert!(decode_for_peer::<IpcOpen>(&bytes, PeerKind::Network).is_err());
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["channel"], "admin");
    let reply = IpcOpenReply::Ready {
        version: PROTOCOL_VERSION,
    };
    assert_eq!(
        decode::<IpcOpenReply>(&encode(&reply).unwrap()).unwrap(),
        reply
    );
    let refused = IpcOpenReply::Refused(
        negotiate(Version { major: 7, minor: 0 })
            .unwrap_err()
            .into_error(),
    );
    assert_eq!(
        decode::<IpcOpenReply>(&encode(&refused).unwrap()).unwrap(),
        refused
    );
}

#[test]
fn cli_read_answer_names_the_broker() {
    let CliReply::Read { broker, .. } = &cli_replies()[5] else {
        panic!()
    };
    assert_eq!(broker, "local");
}

#[test]
fn resource_override_fields_are_optional_for_older_senders() {
    let CliRequest::Submit(mut submit) = cli_requests().remove(0) else {
        panic!()
    };
    let old = serde_json::to_value(&submit).unwrap();
    assert!(old.get("mem_mb").is_none() && old.get("cpu").is_none());
    let back: SubmitRequest = serde_json::from_value(old).unwrap();
    assert_eq!((back.mem_mb, back.cpu), (None, None));
    submit.mem_mb = Some(6144);
    submit.cpu = Some(4);
    let v = serde_json::to_value(&submit).unwrap();
    assert_eq!(
        (v["mem_mb"].as_u64(), v["cpu"].as_u64()),
        (Some(6144), Some(4))
    );
    assert_eq!(serde_json::from_value::<SubmitRequest>(v).unwrap(), submit);

    let mut spec = order_spec();
    let old = serde_json::to_value(&spec).unwrap();
    assert!(old.get("overrides").is_none());
    let back: OrderSpec = serde_json::from_value(old).unwrap();
    assert!(back.overrides.is_empty());
    spec.overrides = ResourceOverride {
        mem_mb: Some(6144),
        cpu: None,
    };
    let v = serde_json::to_value(&spec).unwrap();
    assert_eq!(v["overrides"], json!({ "mem_mb": 6144 }));
    assert_eq!(serde_json::from_value::<OrderSpec>(v).unwrap(), spec);
}
