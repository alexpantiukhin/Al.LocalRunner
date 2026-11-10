#![allow(dead_code)]

use super::*;

pub fn read_requests() -> Vec<ReadRequest> {
    vec![
        ReadRequest::Status(StatusFilter {
            project: Some("p".into()),
            source: Some(WireSource::Github),
            label: Some("l".into()),
            limit: Some(10),
            cursor: Some("c1".into()),
        }),
        ReadRequest::History(HistoryFilter {
            client: Some("k".into()),
            project: None,
            source: Some(WireSource::Session),
            label: None,
            profile: Some("build".into()),
            state: Some(JobState::Failed),
            platform: None,
            runner: Some("r-1".into()),
            since: Some(Timestamp(5)),
            active_only: true,
            limit: Some(20),
            cursor: Some("c2".into()),
            mine: true,
        }),
        ReadRequest::HistoryShow {
            job: job(1),
            client: Some("local:bob".into()),
        },
        ReadRequest::Explain {
            job: job(1),
            client: None,
        },
        ReadRequest::Log {
            job: job(1),
            step: Some("build".into()),
            tail: Some(50),
            follow: true,
            client: None,
        },
        ReadRequest::Stats(StatsFilter {
            client: None,
            project: Some("p".into()),
            profile: None,
            since: None,
        }),
        ReadRequest::Runners,
        ReadRequest::Subscribe {
            topics: vec![Topic::Queue, Topic::Runners],
        },
        ReadRequest::Unsubscribe { sub: 4 },
        ReadRequest::Unfollow { req: 9 },
    ]
}

pub fn read_replies() -> Vec<ReadReply> {
    vec![
        ReadReply::Status(StatusView {
            running: vec![RunningView {
                summary: summary(),
                steps: vec![step_view()],
                last_lines: vec!["line".into()],
            }],
            queue: vec![QueueItem {
                summary: summary(),
                position: 1,
                wait_reason: Some("runner busy".into()),
            }],
            recent: vec![summary()],
            pool: PoolLoad {
                used_mem_mb: 1,
                total_mem_mb: 2,
                used_cpu: 3,
                total_cpu: 4,
            },
            next: Some("q2".into()),
        }),
        ReadReply::History {
            jobs: vec![summary()],
            next: None,
        },
        ReadReply::Job(Box::new(JobDetail {
            summary: summary(),
            note: Some("n".into()),
            branch: Some("main".into()),
            cwd: Some("/w".into()),
            commit: Some("abc".into()),
            os_user: Some("alex".into()),
            source: Some(WireSource::Github),
            github_run_url: Some("https://example/run/1".into()),
            steps: vec![step_view()],
            secrets: vec!["registry_auth".into()],
            cancelled_by: Some("dashboard:alex".into()),
            wait_reason: None,
        })),
        ReadReply::Explain {
            job: job(1),
            text: "waits for rule 2".into(),
        },
        ReadReply::Stats(stats()),
        ReadReply::Runners {
            runners: vec![runner_info()],
        },
        ReadReply::Subscribed { sub: 4 },
        ReadReply::Unsubscribed { sub: 4 },
        ReadReply::NotFound {
            what: "job-9".into(),
        },
        ReadReply::Denied {
            reason: "not the owner".into(),
        },
        ReadReply::Failed {
            message: "boom".into(),
        },
    ]
}

pub fn events() -> Vec<QueueEvent> {
    vec![
        QueueEvent::JobQueued { job: summary() },
        QueueEvent::JobStarted {
            job: job(1),
            runner: "r-1".into(),
        },
        QueueEvent::StepChanged {
            job: job(1),
            step: step_view(),
        },
        QueueEvent::JobFinished {
            job: job(1),
            state: JobState::Lost,
            reason: Some(ReasonCode::RunnerLost),
        },
        QueueEvent::RunnerOnline { name: "r-1".into() },
        QueueEvent::RunnerOffline { name: "r-1".into() },
        QueueEvent::ServiceChanged {
            runner: "r-1".into(),
            service: service_report(),
        },
    ]
}
