mod common;

use common::*;
use lr_abstractions::fakes::{JobScript, StepScript};
use lr_abstractions::{CacheEntry, CacheKey, Level, RunnerStore};
use lr_core::{JobState, Timestamp};
use lr_proto::ReasonCode;
use lr_runner::Retention;
use std::path::Path;
use std::time::Duration;

const DAY: u64 = 86_400_000;

fn tree_job(disk_mb: u64, command: &str) -> lr_proto::AssignSpec {
    let mut spec = worktree_spec("/w/tree", vec![step("b", command, 600)], 600);
    spec.requirements.disk_mb = disk_mb;
    spec
}

#[tokio::test]
async fn a_volume_that_cannot_be_measured_is_not_checked_and_says_so() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, tree_job(500, "make")).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Passed);
    assert_eq!(rig.process.runs().len(), 1);
    let warned: Vec<_> = rig
        .log
        .records()
        .into_iter()
        .filter(|r| r.message == "free disk not measured, disk_mb of the job not checked")
        .collect();
    assert_eq!(warned.len(), 1);
    assert_eq!(warned[0].level, Level::Warn);
    assert_eq!(
        warned[0].fields,
        vec![("path".to_string(), "/w/tree".to_string())]
    );
}

#[tokio::test]
async fn the_disk_check_comes_after_the_cache_cleanup() {
    let mut rig = Rig::new();
    rig.settings.cache = Retention {
        max_size_gb: None,
        retention_days: Some(1),
    };
    rig.files.mkdir(Path::new("/w/tree"));
    rig.disk.set(Path::new("/"), 400);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let stale = CacheEntry {
        key: CacheKey {
            client: "client-1".into(),
            project: "acme/app".into(),
            name: "deps".into(),
        },
        path: "/state/runner-a/cache/client-1/deps".into(),
        bytes: 5,
        last_used: Timestamp(START - 3 * DAY),
    };
    rig.files.put(&Path::new(&stale.path).join("f"), b"x");
    rig.store.upsert_cache(stale).await.unwrap();

    b.assign("job-1", 1, tree_job(500, "make")).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Failed);
    assert_eq!(r.reason, Some(ReasonCode::DiskInsufficient));
    let detail = r.detail.unwrap_or_default();
    assert!(detail.contains("after the cache cleanup"), "{detail}");
    assert!(
        rig.store.cache_entries().await.unwrap().is_empty(),
        "the stale entry is swept before the job is refused"
    );
    assert!(rig.process.runs().is_empty());
}

#[tokio::test]
async fn job_output_and_commands_never_reach_a_record() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script(
        "job-1",
        JobScript::new(vec![
            StepScript::new(Duration::ZERO, 3).output(b"out-5e1d password=hunter2\n")
        ]),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, tree_job(0, "echo token=cmd-91ab"))
        .await;
    let msgs = b.until_result().await;
    assert_eq!(result_of(&msgs).state, JobState::Failed);
    assert!(
        chunks_text(&msgs).contains("out-5e1d"),
        "the output went out"
    );
    let records = rig.log.records();
    assert!(
        records
            .iter()
            .any(|r| r.fields.iter().any(|(_, v)| v == "job-1")),
        "the job is logged at all: {}",
        rig.logs_text()
    );
    for r in &records {
        let text = format!("{} {:?}", r.message, r.fields);
        for secret in ["out-5e1d", "hunter2", "cmd-91ab", "echo token"] {
            assert!(!text.contains(secret), "{secret} leaked into {text}");
        }
    }
}

#[tokio::test]
async fn a_successful_connection_to_the_broker_is_logged_without_secrets() {
    let rig = Rig::new();
    let (_c, _h) = rig.start();
    let _b = Broker::accept(&rig).await;
    let mut connected = Vec::new();
    for _ in 0..200 {
        connected = rig
            .log
            .records()
            .into_iter()
            .filter(|r| r.message == "broker connected")
            .collect();
        if !connected.is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(connected.len(), 1, "{}", rig.logs_text());
    assert_eq!(connected[0].level, Level::Info);
    assert_eq!(
        connected[0].fields,
        vec![("address".to_string(), BROKER.to_string())]
    );
    let text = format!("{:?}", connected[0].fields);
    assert!(!text.contains(FINGERPRINT) && !text.contains("runner-key"));
}
