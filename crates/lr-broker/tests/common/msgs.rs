use super::harness::START;
use lr_abstractions::{Peer, PeerAccess};
use lr_core::{Endpoint, Identity, JobId, MachineRef, Requirements, Source, StepSpec, Timestamp};
use lr_proto::*;
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::path::PathBuf;

pub fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

pub fn local_peer(os_user: &str) -> Peer {
    Peer {
        endpoint: Endpoint::Ipc {
            os_user: os_user.into(),
        },
        key: None,
        access: PeerAccess::Local,
    }
}

pub fn member_peer(key: &str, addr: &str) -> Peer {
    Peer {
        endpoint: Endpoint::Addr(ip(addr)),
        key: Some(key.into()),
        access: PeerAccess::Member,
    }
}

pub fn hello(name: &str, caps: Option<WireCapabilities>) -> HubToBroker {
    HubToBroker::Hello(Hello {
        version: PROTOCOL_VERSION,
        name: name.into(),
        capabilities: caps,
    })
}

pub fn caps(platforms: &[&str], mem_mb: u64, cpu: u32) -> WireCapabilities {
    WireCapabilities {
        platforms: platforms.iter().map(|s| s.to_string()).collect(),
        mem_mb,
        cpu,
        disk_mb: 100_000,
        programs: BTreeSet::new(),
        tags: BTreeSet::new(),
        max_job_timeout_s: None,
        services: BTreeMap::new(),
        machine: None,
        accept_min_mem_mb: None,
        cache_max_mb: None,
        cache_used_mb: None,
        service_mem_mb: Default::default(),
        commands: Default::default(),
        enforcement: None,
        version: None,
    }
}

pub fn linux() -> WireCapabilities {
    caps(&["linux"], 16384, 8)
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
        commands: Default::default(),
    }
}

pub fn same_machine_req() -> Requirements {
    let mut r = req(512, 1, 60);
    r.platform = None;
    r.same_machine = Some(MachineRef("whatever-the-hub-says".into()));
    r
}

pub fn identity(label: &str) -> Identity {
    Identity {
        label: label.into(),
        note: Some("why".into()),
        os_user: "alex".into(),
        host: "laptop".into(),
        sender_pid: 7,
        sender_start: Some(1),
        cwd: PathBuf::from("/work/repo"),
        branch: Some("main".into()),
        worktree_root: None,
        repo_root: None,
        project: "owner/repo".into(),
        source: Source::Session,
    }
}

pub fn order_at(job: &str, requirements: Requirements, enqueued_ms: u64) -> HubToBroker {
    HubToBroker::Order(Order {
        job: JobId(job.into()),
        seq: 1,
        spec: Box::new(OrderSpec {
            profile: "unit".into(),
            requirements,
            steps: vec![StepSpec {
                name: "build".into(),
                command: "make".into(),
                timeout_s: 60,
                mem_mb: None,
                cpu: None,
                tags: None,
                secrets: Vec::new(),
                continue_on_failure: false,
            }],
            enqueued_at: Timestamp(enqueued_ms),
            identity: identity("agent-1"),
            code: WireCode::Commit {
                sha: "abc".into(),
                repo_url: None,
                branch: None,
            },
            wanted_priority: None,
            overrides: ResourceOverride::default(),
            args: Vec::new(),
            labels: Vec::new(),
        }),
    })
}

pub fn order(job: &str, requirements: Requirements) -> HubToBroker {
    order_at(job, requirements, START)
}

pub fn cancel(job: &str, seq: u64) -> HubToBroker {
    HubToBroker::Cancel(HubCancel {
        job: JobId(job.into()),
        seq,
        by: Box::new(identity("agent-1")),
        force: false,
        code: ReasonCode::CancelledByUser,
        reason: Some("stop".into()),
        client: None,
    })
}

pub fn started(job: &str, seq: u64) -> RunnerToBroker {
    RunnerToBroker::Started(Started {
        job: JobId(job.into()),
        seq,
    })
}

pub fn result(job: &str, seq: u64, state: lr_core::JobState, exit: i32) -> RunnerToBroker {
    RunnerToBroker::JobResult(JobResult {
        job: JobId(job.into()),
        seq,
        state,
        exit_code: Some(exit),
        reason: None,
        detail: None,
        peak_disk_mb: None,
        peak_mem_mb: None,
    })
}

pub fn chunk(job: &str, seq: u64, text: &str) -> RunnerToBroker {
    RunnerToBroker::Chunk(Chunk {
        job: JobId(job.into()),
        seq,
        step: Some(0),
        text: text.into(),
    })
}
