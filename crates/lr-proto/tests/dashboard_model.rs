mod common;

use common::*;
use lr_core::{Timestamp, Zone};
use lr_proto::*;

fn detail() -> JobDetail {
    JobDetail {
        summary: summary(),
        note: Some("check".into()),
        branch: Some("main".into()),
        cwd: Some("/work/repo".into()),
        commit: None,
        os_user: Some("alex".into()),
        source: Some(WireSource::Session),
        github_run_url: None,
        steps: vec![step_view()],
        secrets: vec!["registry_auth".into()],
        cancelled_by: None,
        wait_reason: Some("runner busy".into()),
    }
}

fn snapshot() -> DashboardSnapshot {
    let lists = [
        DashboardList::Running,
        DashboardList::Queued,
        DashboardList::Outbox,
        DashboardList::History,
    ];
    DashboardSnapshot {
        taken_at: Timestamp(1_700_000_000_000),
        brokers: vec![BrokerStatus {
            name: "home".into(),
            zone: Some(Zone::Lan),
            available: false,
            outbox: 1,
            since: None,
        }],
        jobs: lists
            .iter()
            .enumerate()
            .map(|(i, list)| DashboardJob {
                broker: (*list != DashboardList::Outbox).then(|| "home".into()),
                list: *list,
                position: Some(i as u32 + 1),
                detail: detail(),
                last_lines: vec!["tail".into()],
                stale: i % 2 == 0,
                wait_code: None,
            })
            .collect(),
        runners: vec![BrokerRunners {
            broker: "home".into(),
            pool: PoolLoad {
                used_mem_mb: 1,
                total_mem_mb: 2,
                used_cpu: 0,
                total_cpu: 1,
            },
            runners: vec![runner_info()],
            stale: true,
        }],
        stats: stats(),
    }
}

#[test]
fn one_snapshot_model_survives_json_for_the_page_the_file_and_the_export() {
    let snap = snapshot();
    let json = serde_json::to_string(&snap).unwrap();
    let back: DashboardSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(back, snap);
    let lists: Vec<String> = serde_json::to_value(&snap).unwrap()["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["list"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(lists, ["running", "queued", "outbox", "history"]);
    let event = DashboardEvent {
        broker: "home".into(),
        event: QueueEvent::RunnerOnline { name: "r1".into() },
    };
    let text = serde_json::to_string(&event).unwrap();
    assert_eq!(
        serde_json::from_str::<DashboardEvent>(&text).unwrap(),
        event
    );
}

#[test]
fn a_job_row_of_an_older_hub_and_a_list_of_a_newer_one_still_read() {
    let mut row = serde_json::to_value(&snapshot().jobs[0]).unwrap();
    let object = row.as_object_mut().unwrap();
    for newer in ["position", "last_lines", "stale"] {
        object.remove(newer);
    }
    object.insert("list".into(), "parked".into());
    let old: DashboardJob = serde_json::from_value(row).unwrap();
    assert_eq!(old.list, DashboardList::Unknown);
    assert_eq!(old.position, None);
    assert!(old.last_lines.is_empty());
    assert!(!old.stale, "a row without the mark is fresh");
    let mut runners = serde_json::to_value(&snapshot().runners[0]).unwrap();
    runners.as_object_mut().unwrap().remove("stale");
    let runners: BrokerRunners = serde_json::from_value(runners).unwrap();
    assert!(!runners.stale);
}
