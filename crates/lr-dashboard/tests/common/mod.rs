#![allow(dead_code)]

pub mod http;

use lr_abstractions::fakes::FakeDataSource;
use lr_core::{JobId, JobState, StepState, Timestamp, Zone};
use lr_proto::{
    BrokerStatus, DashboardEvent, DashboardJob, DashboardList, DashboardSnapshot, JobDetail,
    JobSummary, StatsView, StepView, WireSource,
};

pub const XSS: &str = "<script>alert(1)</script>\"'&";

pub type Source = FakeDataSource<DashboardSnapshot, DashboardEvent>;

fn job(
    id: &str,
    list: DashboardList,
    state: JobState,
    project: &str,
    source: WireSource,
    label: &str,
) -> DashboardJob {
    DashboardJob {
        broker: Some("home".into()),
        list,
        position: (list == DashboardList::Queued).then_some(1),
        detail: JobDetail {
            summary: JobSummary {
                job: JobId(id.into()),
                client_label: "alex@laptop".into(),
                state,
                platform: Some("windows".into()),
                runner: (state == JobState::Running).then(|| "box-1".into()),
                queued_at: Timestamp(1_700_000_000_000),
                started_at: None,
                finished_at: None,
                exit_code: None,
                reason: None,
                project: Some(project.into()),
                label: Some(label.into()),
                profile: Some("unit".into()),
            },
            note: Some(XSS.into()),
            branch: Some("main".into()),
            cwd: Some("D:/work/alpha".into()),
            commit: None,
            os_user: Some("alex".into()),
            source: Some(source),
            github_run_url: None,
            steps: vec![StepView {
                ord: 0,
                name: "build".into(),
                state: StepState::Queued,
                exit_code: None,
                started_at: None,
                finished_at: None,
                timeout_s: 600,
            }],
            secrets: vec![],
            cancelled_by: None,
            wait_reason: (state == JobState::Queued).then(|| "runner busy".into()),
        },
        last_lines: if state == JobState::Running {
            vec!["tail secret line".to_string()]
        } else {
            Vec::new()
        },
        stale: false,
    }
}

/// A snapshot with a running job, a queued job and a queued job whose project and label are
/// hostile markup.
pub fn snapshot() -> DashboardSnapshot {
    DashboardSnapshot {
        taken_at: Timestamp(1_700_000_100_000),
        brokers: vec![BrokerStatus {
            name: "home".into(),
            zone: Some(Zone::Lan),
            available: true,
            outbox: 0,
        }],
        jobs: vec![
            job(
                "j-run",
                DashboardList::Running,
                JobState::Running,
                "beta",
                WireSource::Session,
                "cli",
            ),
            job(
                "j-queued",
                DashboardList::Queued,
                JobState::Queued,
                "alpha",
                WireSource::Github,
                "agent-1",
            ),
            job(
                "j-xss",
                DashboardList::Queued,
                JobState::Queued,
                XSS,
                WireSource::Session,
                XSS,
            ),
        ],
        runners: vec![],
        stats: StatsView {
            profiles: vec![],
            clients: vec![],
        },
    }
}

pub fn source() -> std::sync::Arc<Source> {
    std::sync::Arc::new(FakeDataSource::new(snapshot()))
}
