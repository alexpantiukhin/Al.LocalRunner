#![allow(dead_code)]

use lr_broker::policy::{load, HostResources, PolicyError, PolicyFile};
use lr_core::{ClientView, Declared, Endpoint, JobId, Order, QueueEntry, Requirements, Timestamp};
use std::collections::BTreeSet;

pub const SPEC_EXAMPLE: &str = include_str!("../fixtures/spec_policy.toml");

pub const HOST: HostResources = HostResources {
    mem_mb: 32768,
    cpu: 16,
};

pub fn req(mem_mb: u64, cpu: u32) -> Requirements {
    Requirements {
        platform: Some("linux".into()),
        timeout_s: 60,
        mem_mb,
        cpu,
        disk_mb: 0,
        programs: Vec::new(),
        tags: BTreeSet::new(),
        exclusive: BTreeSet::new(),
        same_machine: None,
        services: Vec::new(),
        secrets: BTreeSet::new(),
        cache: Vec::new(),
        max_zone: None,
    }
}

pub fn entry_with(
    id: &str,
    label: &str,
    enqueued_ms: u64,
    requirements: Requirements,
) -> QueueEntry {
    QueueEntry {
        order: Order {
            job: JobId(id.into()),
            requirements,
            enqueued_at: Timestamp(enqueued_ms),
            declared: Declared::default(),
            wanted_priority: None,
        },
        client: ClientView {
            key: format!("key-{id}"),
            label: label.into(),
            machine_id: format!("m-{id}"),
            endpoint: Endpoint::Ipc {
                os_user: "u".into(),
            },
        },
        accepted_at: Timestamp(enqueued_ms),
    }
}

pub fn entry(id: &str, label: &str, enqueued_ms: u64) -> QueueEntry {
    entry_with(id, label, enqueued_ms, req(512, 1))
}

pub fn ok(text: &str) -> PolicyFile {
    load(text).unwrap_or_else(|e| panic!("policy must load: {e}"))
}

pub fn err(text: &str) -> PolicyError {
    match load(text) {
        Ok(_) => panic!("policy must be refused:\n{text}"),
        Err(e) => e,
    }
}
