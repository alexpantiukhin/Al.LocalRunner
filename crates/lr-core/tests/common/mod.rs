#![allow(dead_code)]

use lr_core::{
    Accept, Capabilities, ClientView, Declared, Endpoint, JobId, MachineRef, Order, PoolLimits,
    QueueEntry, Requirements, RulePolicy, RunnerView, RunningJob, Site, Timestamp, Zone,
};
use std::collections::{BTreeMap, BTreeSet};

pub const MIN: u64 = 60_000;

pub fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

pub fn req(mem_mb: u64, cpu: u32, timeout_s: u64) -> Requirements {
    Requirements {
        platform: Some("linux".into()),
        timeout_s,
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

pub fn pool(mem_mb: u64, cpu: u32) -> PoolLimits {
    PoolLimits {
        mem_mb,
        cpu,
        exclusive_tags: BTreeSet::new(),
    }
}

pub fn policy(mem_mb: u64, cpu: u32) -> RulePolicy {
    RulePolicy::with_pool(pool(mem_mb, cpu))
}

pub fn client(label: &str) -> ClientView {
    ClientView {
        key: format!("key-{label}"),
        label: label.into(),
        machine_id: "m-client".into(),
        endpoint: Endpoint::Addr("192.168.1.50".parse().unwrap()),
    }
}

pub fn entry_with(
    job: &str,
    label: &str,
    enqueued_ms: u64,
    requirements: Requirements,
) -> QueueEntry {
    QueueEntry {
        order: Order {
            job: JobId(job.into()),
            requirements,
            enqueued_at: Timestamp(enqueued_ms),
            declared: Declared::default(),
            wanted_priority: None,
        },
        client: client(label),
        accepted_at: Timestamp(enqueued_ms),
    }
}

pub fn entry(job: &str, label: &str, enqueued_ms: u64) -> QueueEntry {
    entry_with(job, label, enqueued_ms, req(1024, 1, 600))
}

pub fn caps() -> Capabilities {
    Capabilities {
        platforms: set(&["linux"]),
        mem_mb: 16384,
        cpu: 8,
        disk_mb: 100_000,
        programs: BTreeSet::new(),
        tags: BTreeSet::new(),
        max_job_timeout_s: None,
        services: BTreeMap::new(),
        zone: Zone::Remote,
        machine: None,
        accept: Accept::default(),
    }
}

pub fn runner_at(name: &str, addr: &str) -> RunnerView {
    RunnerView {
        name: name.into(),
        caps: caps(),
        endpoint: Endpoint::Addr(addr.parse().unwrap()),
        online: true,
    }
}

pub fn runner(name: &str) -> RunnerView {
    runner_at(name, "192.168.1.20")
}

pub fn ipc_runner(name: &str, os_user: &str) -> RunnerView {
    RunnerView {
        name: name.into(),
        caps: Capabilities {
            machine: Some(MachineRef(os_user.into())),
            ..caps()
        },
        endpoint: Endpoint::Ipc {
            os_user: os_user.into(),
        },
        online: true,
    }
}

pub fn running(entry: QueueEntry, runner: &str, started_ms: u64) -> RunningJob {
    RunningJob {
        entry,
        runner: runner.into(),
        started_at: Timestamp(started_ms),
    }
}

pub fn site() -> Site {
    let mut networks = BTreeMap::new();
    networks.insert(
        "home".to_string(),
        vec![
            "192.168.1.0/24".parse().unwrap(),
            "127.0.0.1/32".parse().unwrap(),
        ],
    );
    networks.insert("office".to_string(), vec!["10.1.0.0/16".parse().unwrap()]);
    Site {
        broker_addrs: vec!["192.168.1.10".parse().unwrap()],
        networks,
    }
}

pub fn job(id: &str) -> JobId {
    JobId(id.into())
}

pub const SEC: u64 = 1_000;

pub fn plan(
    p: &RulePolicy,
    queue: &[QueueEntry],
    runners: &[RunnerView],
    run: &[RunningJob],
    now: u64,
) -> lr_core::Plan {
    let served = lr_core::ServeLog::default();
    let state = lr_core::QueueState {
        queue,
        runners,
        running: run,
        served: &served,
    };
    lr_core::pick_next(p, &site(), &state, Timestamp(now))
}

pub fn started(p: &lr_core::Plan) -> Vec<(String, String)> {
    p.assignments
        .iter()
        .map(|a| (a.job.0.clone(), a.runner.clone()))
        .collect()
}

pub fn wait_of<'a>(p: &'a lr_core::Plan, id: &str) -> Option<&'a lr_core::Wait> {
    p.waiting.iter().find(|(j, _)| j.0 == id).map(|(_, w)| w)
}

pub fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}
