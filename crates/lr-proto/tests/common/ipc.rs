#![allow(dead_code)]

use super::*;

pub fn cli_requests() -> Vec<CliRequest> {
    vec![
        CliRequest::Submit(SubmitRequest {
            identity: identity(),
            profile: Some("build".into()),
            platform: Some("linux".into()),
            timeout_s: Some(60),
            mem_mb: None,
            cpu: None,
            args: Vec::new(),
            detach: true,
            wait: false,
        }),
        CliRequest::Done { job: job(1) },
        CliRequest::Wait { job: job(1) },
        CliRequest::Cancel(CancelRequest {
            actor: identity(),
            target: CancelTarget::Job(job(1)),
            force: true,
            reason: Some("r".into()),
        }),
        CliRequest::Cancel(CancelRequest {
            actor: identity(),
            target: CancelTarget::Mine,
            force: false,
            reason: None,
        }),
        CliRequest::Read(CliRead {
            broker: Some("local".into()),
            request: ReadRequest::Runners,
        }),
        CliRequest::Brokers,
        CliRequest::Jobs {
            label: "agent".into(),
            active: true,
        },
        CliRequest::Orders,
        CliRequest::Shutdown,
        CliRequest::LoggingReload,
        CliRequest::Events,
    ]
}

pub fn cli_replies() -> Vec<CliReply> {
    vec![
        CliReply::Submitted {
            job: job(1),
            warnings: vec!["warning: uncommitted changes are ignored".into()],
        },
        CliReply::Rejected {
            code: ReasonCode::Rejected(RejectReason::NoLocalBroker),
            detail: "no local broker".into(),
        },
        CliReply::Output {
            job: job(1),
            text: "t".into(),
        },
        CliReply::Exit {
            job: job(1),
            state: JobState::Passed,
            exit_code: Some(0),
            reason: None,
        },
        CliReply::Cancelled {
            jobs: vec![job(1), job(2)],
        },
        CliReply::Read {
            broker: "local".into(),
            reply: ReadReply::Runners { runners: vec![] },
        },
        CliReply::Brokers {
            brokers: vec![BrokerStatus {
                name: "local".into(),
                zone: Some(Zone::Local),
                available: true,
                outbox: 2,
            }],
        },
        CliReply::Jobs { jobs: vec![] },
        CliReply::Orders {
            orders: vec![HubOrder {
                broker: None,
                outbox: true,
                summary: summary(),
                log: Some("/w/temp/runner/j1.log".into()),
            }],
            truncated: true,
        },
        CliReply::LogChunk {
            broker: "local".into(),
            text: "line\n".into(),
            end: false,
        },
        CliReply::Stopping,
        CliReply::Reloaded,
        CliReply::Watching,
        CliReply::Event(DashboardEvent {
            broker: "local".into(),
            event: QueueEvent::JobStarted {
                job: lr_core::JobId("j-1".into()),
                runner: "r1".into(),
            },
        }),
        CliReply::Error(protocol_error()),
    ]
}

pub fn admin_requests() -> Vec<AdminRequest> {
    vec![
        AdminRequest::PolicyReload,
        AdminRequest::PolicyCheck,
        AdminRequest::LoggingReload,
        AdminRequest::BrokerAdd {
            name: Some("ci".into()),
            label: Some("agent".into()),
            runners: Some(3),
            address: None,
        },
        AdminRequest::RunnerRemove { name: "r-1".into() },
        AdminRequest::ClientRemove { name: "ci".into() },
        AdminRequest::RunnerList,
        AdminRequest::ClientList,
        AdminRequest::Shutdown,
    ]
}

pub fn admin_replies() -> Vec<AdminReply> {
    vec![
        AdminReply::Ok {
            warnings: vec!["rule 2 shadowed".into()],
        },
        AdminReply::Policy {
            path: "/var/lib/al-localrunner/policy.toml".into(),
            warnings: vec!["rule 2 shadowed".into()],
            rules: vec!["rule 1: when everyone".into()],
        },
        AdminReply::JoinCode {
            address: "host:7000".into(),
            code: "tok".into(),
            fingerprint: "ab:cd".into(),
            expires_at: Timestamp(5),
        },
        AdminReply::Names {
            names: vec!["a".into()],
        },
        AdminReply::Failed {
            message: "denied".into(),
        },
    ]
}

pub fn controls() -> Vec<RunnerControl> {
    vec![
        RunnerControl::Down {
            mode: DownMode::Drain,
        },
        RunnerControl::Down {
            mode: DownMode::Cancel,
        },
        RunnerControl::LoggingReload,
    ]
}

pub fn control_replies() -> Vec<ControlReply> {
    vec![
        ControlReply::Ok,
        ControlReply::Failed {
            message: "x".into(),
        },
    ]
}
