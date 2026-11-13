#![allow(dead_code)]

mod ipc;
mod views;
#[allow(unused_imports)]
pub use ipc::*;
#[allow(unused_imports)]
pub use views::*;

use lr_core::{
    Identity, JobId, JobState, MachineRef, Program, Requirements, Source, StepSpec, StepState,
    Timestamp, Zone,
};
use lr_proto::*;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub fn job(n: u32) -> JobId {
    JobId(format!("job-{n}"))
}

pub fn identity() -> Identity {
    Identity {
        label: "agent-1".into(),
        note: Some("check".into()),
        os_user: "alex".into(),
        host: "laptop".into(),
        sender_pid: 42,
        sender_start: Some(7),
        cwd: PathBuf::from("/work/repo"),
        branch: Some("main".into()),
        worktree_root: None,
        repo_root: None,
        project: "owner/repo".into(),
        source: Source::Session,
    }
}

pub fn requirements() -> Requirements {
    Requirements {
        platform: Some("linux".into()),
        timeout_s: 600,
        mem_mb: 1024,
        cpu: 2,
        disk_mb: 100,
        programs: vec![Program {
            name: "git".into(),
            check: Some("git --version".into()),
        }],
        tags: ["gradle".to_string()].into(),
        exclusive: ["emulator".to_string()].into(),
        same_machine: Some(MachineRef("m-1".into())),
        services: vec!["build-daemon".into()],
        secrets: ["registry_auth".to_string()].into(),
        cache: vec!["gradle".into()],
        max_zone: Some(Zone::Lan),
        commands: Default::default(),
    }
}

pub fn steps() -> Vec<StepSpec> {
    vec![StepSpec {
        name: "build".into(),
        command: "cargo build".into(),
        timeout_s: 300,
        mem_mb: Some(512),
        cpu: None,
        tags: Some(vec!["extra".into()]),
        secrets: vec!["registry_auth".into()],
        continue_on_failure: true,
    }]
}

pub fn caps() -> WireCapabilities {
    WireCapabilities {
        platforms: ["linux".to_string(), "android".to_string()].into(),
        mem_mb: 16384,
        cpu: 8,
        disk_mb: 50_000,
        programs: ["git".to_string()].into(),
        tags: ["gradle".to_string()].into(),
        max_job_timeout_s: Some(3600),
        services: BTreeMap::from([
            ("build-daemon".to_string(), WireServiceStatus::Available),
            ("emulator".to_string(), WireServiceStatus::Unavailable),
        ]),
        machine: Some(MachineRef("m-1".into())),
        accept_min_mem_mb: Some(8192),
        cache_max_mb: Some(40_000),
        cache_used_mb: Some(1200),
        service_mem_mb: Default::default(),
        commands: Default::default(),
    }
}

pub fn order_spec() -> OrderSpec {
    OrderSpec {
        profile: "build".into(),
        requirements: requirements(),
        steps: steps(),
        enqueued_at: Timestamp(1_700_000_000_000),
        identity: identity(),
        code: WireCode::Commit {
            sha: "abc123".into(),
            repo_url: None,
            branch: None,
        },
        wanted_priority: Some(5),
        overrides: ResourceOverride::default(),
        args: Vec::new(),
    }
}

pub fn assign_spec() -> AssignSpec {
    AssignSpec {
        client: "laptop".into(),
        project: "owner/repo".into(),
        requirements: requirements(),
        steps: steps(),
        secrets: vec!["registry_auth".into()],
        code: WireCode::WorkTree {
            cwd: "/work/repo".into(),
        },
        broker_lost_kill_s: Some(180),
        named_commands: false,
        cache_client: None,
    }
}

pub fn service_report() -> ServiceReport {
    ServiceReport {
        name: "build-daemon".into(),
        state: ServiceState::Idle,
        mem_mb: 2048,
        remaining_s: Some(900),
    }
}

pub fn summary() -> JobSummary {
    JobSummary {
        job: job(1),
        client_label: "ci".into(),
        state: JobState::Running,
        platform: Some("linux".into()),
        runner: Some("r-1".into()),
        queued_at: Timestamp(1),
        started_at: Some(Timestamp(2)),
        finished_at: None,
        exit_code: None,
        reason: Some(ReasonCode::CancelledByUser),
        project: Some("owner/repo".into()),
        label: Some("agent-1".into()),
        profile: Some("build".into()),
        source: Some(WireSource::Session),
        note: None,
        branch: None,
        cwd: None,
        timeout_s: None,
        github_run_url: None,
    }
}

pub fn step_view() -> StepView {
    StepView {
        ord: 0,
        name: "build".into(),
        state: StepState::Running,
        exit_code: None,
        started_at: Some(Timestamp(3)),
        finished_at: None,
        timeout_s: 300,
    }
}

pub fn runner_info() -> RunnerInfo {
    RunnerInfo {
        name: "r-1".into(),
        online: true,
        caps: caps(),
        last_seen: Some(Timestamp(9)),
        free_mem_mb: 4096,
        free_cpu: 4,
        held_tags: vec![HeldTag {
            tag: "emulator".into(),
            job: job(1),
            exclusive: true,
        }],
        services: vec![service_report()],
        unmet: vec!["adb not found".into()],
    }
}

pub fn spread() -> SpreadView {
    SpreadView {
        median: Some(10),
        p95: None,
    }
}

pub fn stats() -> StatsView {
    StatsView {
        profiles: vec![ProfileStatsView {
            profile: "build".into(),
            runs: 3,
            passed: 2,
            failed: 1,
            timeouts: 0,
            cancelled: 0,
            lost: 0,
            wait_ms: spread(),
            duration_ms: spread(),
            steps: vec![StepStatsView {
                name: "build".into(),
                runs: 3,
                passed: 2,
                duration_ms: spread(),
            }],
        }],
        clients: vec![ClientUsageView {
            client: "k1".into(),
            label: "ci".into(),
            runs: 3,
            busy_ms: 99,
        }],
    }
}

pub fn started(n: u32, seq: u64) -> Started {
    Started { job: job(n), seq }
}

pub fn chunk(seq: u64) -> Chunk {
    Chunk {
        job: job(1),
        seq,
        step: Some(0),
        text: "hello\n".into(),
    }
}

pub fn step_result(seq: u64) -> StepResult {
    StepResult {
        job: job(1),
        seq,
        ord: 0,
        name: "build".into(),
        state: StepState::Passed,
        exit_code: Some(0),
        duration_ms: 1500,
    }
}

pub fn job_result(seq: u64) -> JobResult {
    JobResult {
        job: job(1),
        seq,
        state: JobState::Failed,
        exit_code: Some(2),
        reason: Some(ReasonCode::RunnerRestarted),
        detail: Some("restarted".into()),
    }
}

pub fn hello() -> Hello {
    Hello {
        version: PROTOCOL_VERSION,
        name: "r-1".into(),
        capabilities: Some(caps()),
    }
}

pub fn join(role: Role) -> Join {
    Join {
        version: PROTOCOL_VERSION,
        role,
        code: "code-123".into(),
        public_key: "pk".into(),
        name: "r-1".into(),
        machine_id: "m-1".into(),
        capabilities: Some(caps()),
    }
}

pub fn welcome() -> Welcome {
    Welcome {
        version: PROTOCOL_VERSION,
        zone: Some(Zone::Lan),
        heartbeat_s: HEARTBEAT_INTERVAL_S,
        broker_lost_kill_s: Some(180),
    }
}

pub fn joined() -> Joined {
    Joined {
        version: PROTOCOL_VERSION,
        zone: Some(Zone::Lan),
        heartbeat_s: HEARTBEAT_INTERVAL_S,
        broker_lost_kill_s: Some(180),
        key_id: "k-1".into(),
        broker_machine_id: "bm-1".into(),
    }
}

pub fn resume() -> Resume {
    Resume {
        points: vec![
            ResumePoint {
                job: job(1),
                last_seq: 7,
            },
            ResumePoint {
                job: job(2),
                last_seq: 0,
            },
        ],
    }
}

pub fn ack(seq: u64) -> Ack {
    Ack { job: job(1), seq }
}

pub fn protocol_error() -> ProtocolError {
    ProtocolError {
        code: ProtocolErrorCode::SeqGap,
        message: "gap".into(),
        supported: Some(PROTOCOL_VERSION),
    }
}

pub fn runner_to_broker() -> Vec<RunnerToBroker> {
    vec![
        RunnerToBroker::Hello(hello()),
        RunnerToBroker::Join(join(Role::Runner)),
        RunnerToBroker::Resume(resume()),
        RunnerToBroker::Ack(ack(3)),
        RunnerToBroker::Heartbeat,
        RunnerToBroker::Drain,
        RunnerToBroker::Bye(Bye {
            reason: Some("down".into()),
        }),
        RunnerToBroker::Started(started(1, 1)),
        RunnerToBroker::Chunk(chunk(2)),
        RunnerToBroker::StepResult(step_result(3)),
        RunnerToBroker::JobResult(job_result(4)),
        RunnerToBroker::ServicesReport(ServicesReport {
            services: vec![service_report()],
        }),
        RunnerToBroker::CapabilitiesUpdate(CapabilitiesUpdate {
            capabilities: caps(),
        }),
        RunnerToBroker::Error(protocol_error()),
    ]
}

pub fn broker_to_runner() -> Vec<BrokerToRunner> {
    vec![
        BrokerToRunner::Welcome(welcome()),
        BrokerToRunner::Joined(joined()),
        BrokerToRunner::Resume(resume()),
        BrokerToRunner::Ack(ack(3)),
        BrokerToRunner::Heartbeat,
        BrokerToRunner::Bye(Bye { reason: None }),
        BrokerToRunner::Assign(Assign {
            job: job(1),
            seq: 1,
            spec: Box::new(assign_spec()),
        }),
        BrokerToRunner::Cancel(RunnerCancel {
            job: job(1),
            seq: 2,
            code: ReasonCode::RunnerStopped,
            detail: Some("down --cancel".into()),
        }),
        BrokerToRunner::StopService(StopService {
            name: "build-daemon".into(),
        }),
        BrokerToRunner::SettingsUpdate(SettingsUpdate {
            broker_lost_kill_s: Some(60),
        }),
        BrokerToRunner::Error(protocol_error()),
    ]
}

pub fn hub_to_broker() -> Vec<HubToBroker> {
    vec![
        HubToBroker::Hello(hello()),
        HubToBroker::Join(join(Role::Hub)),
        HubToBroker::CapabilitiesQuery(CapabilitiesQuery { req: 1 }),
        HubToBroker::Order(Order {
            job: job(1),
            seq: 1,
            spec: Box::new(order_spec()),
        }),
        HubToBroker::Cancel(HubCancel {
            job: job(1),
            seq: 2,
            by: Box::new(identity()),
            force: true,
            code: ReasonCode::SenderGone,
            reason: Some("stuck".into()),
            client: Some("local:bob".into()),
        }),
        HubToBroker::Resume(resume()),
        HubToBroker::Ack(ack(5)),
        HubToBroker::Heartbeat,
        HubToBroker::Bye(Bye { reason: None }),
        HubToBroker::Read(ReadCall {
            req: 9,
            request: ReadRequest::Runners,
        }),
        HubToBroker::Error(protocol_error()),
    ]
}

pub fn broker_to_hub() -> Vec<BrokerToHub> {
    vec![
        BrokerToHub::Welcome(welcome()),
        BrokerToHub::Joined(joined()),
        BrokerToHub::CapabilitiesReply(CapabilitiesReply {
            req: 1,
            zone: Zone::Remote,
            runners: vec![RunnerCaps {
                name: "r-1".into(),
                online: false,
                zone: Zone::Local,
                caps: caps(),
            }],
        }),
        BrokerToHub::Accepted(Accepted {
            job: job(1),
            order_seq: 1,
            waiting: Some("runner busy".into()),
            timeout_trimmed_to: Some(600),
        }),
        BrokerToHub::Rejected(Rejected {
            job: job(1),
            order_seq: 1,
            code: ReasonCode::Rejected(RejectReason::PoolMemory),
            detail: "need 9 MB, pool 8".into(),
        }),
        BrokerToHub::Started(StartedNotice {
            job: job(1),
            seq: 1,
            runner: "r-1".into(),
            started_at: Timestamp(77),
        }),
        BrokerToHub::Chunk(chunk(2)),
        BrokerToHub::StepResult(step_result(3)),
        BrokerToHub::JobResult(job_result(4)),
        BrokerToHub::CancelAck(CancelAck {
            job: job(1),
            seq: 5,
            cancel_seq: 2,
            outcome: CancelOutcome::Denied,
            denial: Some("not the owner".into()),
        }),
        BrokerToHub::Returned(Returned {
            job: job(1),
            seq: 6,
            code: ReasonCode::RunnerRemoved,
            detail: Some("runner removed".into()),
        }),
        BrokerToHub::ReadReply(ReadAnswer {
            req: 9,
            reply: ReadReply::Subscribed { sub: 1 },
        }),
        BrokerToHub::LogChunk(LogChunk {
            req: 9,
            text: "x".into(),
            end: true,
        }),
        BrokerToHub::Event(EventMsg {
            sub: 1,
            event: QueueEvent::RunnerOnline { name: "r-1".into() },
        }),
        BrokerToHub::Resume(resume()),
        BrokerToHub::Ack(ack(5)),
        BrokerToHub::Heartbeat,
        BrokerToHub::Bye(Bye { reason: None }),
        BrokerToHub::Error(protocol_error()),
    ]
}
