mod common;

use common::*;
use lr_core::{Accept, Capabilities, ServiceStatus, Zone};
use lr_proto::*;
use std::collections::{BTreeMap, BTreeSet};

fn all_seqs_runner() -> Vec<(String, u64)> {
    runner_to_broker()
        .iter()
        .filter_map(|m| m.sequence().map(|(j, s)| (j.0.clone(), s)))
        .collect()
}

#[test]
fn seq_bearing_messages_expose_job_and_seq() {
    assert_eq!(all_seqs_runner().len(), 4);
    let to_runner: Vec<_> = broker_to_runner()
        .iter()
        .filter_map(|m| m.sequence().map(|(_, s)| s))
        .collect();
    assert_eq!(to_runner, vec![1, 2]);
    let hub: Vec<_> = hub_to_broker()
        .iter()
        .filter_map(|m| m.sequence().map(|(_, s)| s))
        .collect();
    assert_eq!(hub, vec![1, 2]);
    let from_broker: Vec<_> = broker_to_hub()
        .iter()
        .filter_map(|m| m.sequence().map(|(_, s)| s))
        .collect();
    assert_eq!(from_broker, vec![1, 2, 3, 4, 5, 6]);
}

#[test]
fn control_and_reply_messages_carry_no_seq() {
    assert!(RunnerToBroker::Heartbeat.sequence().is_none());
    assert!(RunnerToBroker::Resume(resume()).sequence().is_none());
    assert!(RunnerToBroker::Ack(ack(3)).sequence().is_none());
    assert!(BrokerToHub::Accepted(Accepted {
        job: job(1),
        order_seq: 1,
        waiting: None
    })
    .sequence()
    .is_none());
    assert!(HubToBroker::Read(ReadCall {
        req: 1,
        request: ReadRequest::Runners
    })
    .sequence()
    .is_none());
}

#[test]
fn dedup_key_is_client_job_seq_and_repeat_is_equal() {
    let a = RunnerToBroker::Chunk(chunk(5));
    let k1 = DedupKey::of("client-a", &a).unwrap();
    let k2 = DedupKey::of("client-a", &a.clone()).unwrap();
    assert_eq!(k1, k2);
    assert_ne!(
        k1,
        DedupKey::of("client-b", &a).unwrap(),
        "another client key"
    );
    assert_ne!(
        k1,
        DedupKey::of("client-a", &RunnerToBroker::Chunk(chunk(6))).unwrap(),
        "another seq"
    );
    let other_job = RunnerToBroker::Started(started(2, 5));
    assert_ne!(
        k1,
        DedupKey::of("client-a", &other_job).unwrap(),
        "another job"
    );
}

#[test]
fn seq_is_counted_per_direction_so_equal_numbers_do_not_collide() {
    let from_hub = DedupKey::of(
        "c",
        &HubToBroker::Cancel(HubCancel {
            job: job(1),
            seq: 2,
            by: Box::new(identity()),
            force: false,
            code: ReasonCode::CancelledByUser,
            reason: None,
            client: None,
        }),
    )
    .unwrap();
    let to_hub = DedupKey::of(
        "c",
        &BrokerToHub::CancelAck(CancelAck {
            job: job(1),
            seq: 2,
            cancel_seq: 2,
            outcome: CancelOutcome::Applied,
            denial: None,
        }),
    )
    .unwrap();
    assert_ne!(from_hub, to_hub);
    assert_eq!(from_hub.direction, Direction::HubToBroker);
    assert_eq!(to_hub.direction, Direction::BrokerToHub);
}

#[test]
fn messages_without_seq_have_no_dedup_key() {
    assert!(DedupKey::of("c", &RunnerToBroker::Heartbeat).is_none());
}

#[test]
fn capabilities_convert_without_loss() {
    let caps = Capabilities {
        platforms: set(["linux".to_string()]),
        mem_mb: 8192,
        cpu: 4,
        disk_mb: 100,
        programs: set(["git".to_string()]),
        tags: set(["t".to_string()]),
        max_job_timeout_s: Some(3600),
        services: BTreeMap::from([
            ("a".to_string(), ServiceStatus::Available),
            ("b".to_string(), ServiceStatus::Unavailable),
        ]),
        zone: Zone::Lan,
        machine: Some(lr_core::MachineRef("m".into())),
        accept: Accept {
            min_mem_mb: Some(4096),
        },
    };
    let wire = WireCapabilities::from(&caps);
    assert_eq!(wire.accept_min_mem_mb, Some(4096));
    assert_eq!(wire.services["b"], WireServiceStatus::Unavailable);
    let json = serde_json::to_vec(&wire).unwrap();
    let back: WireCapabilities = serde_json::from_slice(&json).unwrap();
    assert_eq!(back.into_capabilities(Zone::Lan), caps);
}

#[test]
fn zone_is_not_trusted_from_the_wire() {
    let v = serde_json::to_value(caps()).unwrap();
    assert!(v.get("zone").is_none());
    assert_eq!(caps().into_capabilities(Zone::Remote).zone, Zone::Remote);
}

#[test]
fn cache_figures_ride_in_capabilities_for_the_runners_page() {
    let c = caps();
    assert_eq!(c.cache_max_mb, Some(40_000));
    assert_eq!(c.max_job_timeout_s, Some(3600));
}

fn set<T: Ord>(items: impl IntoIterator<Item = T>) -> BTreeSet<T> {
    items.into_iter().collect()
}

#[test]
fn commit_code_without_repository_reads_as_before() {
    let old: lr_proto::WireCode =
        serde_json::from_str(r#"{"kind":"commit","sha":"abc1234"}"#).unwrap();
    assert_eq!(
        old,
        lr_proto::WireCode::Commit {
            sha: "abc1234".into(),
            repo_url: None,
            branch: None,
        }
    );
    assert_eq!(
        serde_json::to_string(&old).unwrap(),
        r#"{"kind":"commit","sha":"abc1234"}"#
    );
    let new = lr_proto::WireCode::Commit {
        sha: "abc1234".into(),
        repo_url: Some("https://h/o/r.git".into()),
        branch: Some("main".into()),
    };
    let text = serde_json::to_string(&new).unwrap();
    assert_eq!(
        text,
        r#"{"kind":"commit","sha":"abc1234","repo_url":"https://h/o/r.git","branch":"main"}"#
    );
    assert_eq!(
        serde_json::from_str::<lr_proto::WireCode>(&text).unwrap(),
        new
    );
}
