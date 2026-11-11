mod common;

use common::*;
use lr_abstractions::fakes::{JobScript, StepScript};
use lr_core::JobState;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SEC: Duration = Duration::from_secs(1);

fn ledger() -> PathBuf {
    PathBuf::from("/state/disk-reservations")
}

fn needing(mut spec: lr_proto::AssignSpec, disk_mb: u64) -> lr_proto::AssignSpec {
    spec.requirements.disk_mb = disk_mb;
    spec
}

fn tree_job(disk_mb: u64) -> lr_proto::AssignSpec {
    needing(
        worktree_spec("/w/tree", vec![step("b", "make", 600)], 600),
        disk_mb,
    )
}

#[tokio::test]
async fn job_without_enough_free_disk_fails_before_running() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.disk.set(Path::new("/"), 400);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, tree_job(500)).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Failed);
    assert_eq!(r.reason, Some(lr_proto::ReasonCode::DiskInsufficient));
    let detail = r.detail.unwrap_or_default();
    assert!(
        detail.contains("needs 500 MB") && detail.contains("400 MB"),
        "{detail}"
    );
    assert!(rig.process.runs().is_empty());
}

#[tokio::test]
async fn disk_reserved_by_other_runners_is_not_free() {
    let mut rig = Rig::new();
    rig.settings.disk_ledger = Some(ledger());
    rig.files.mkdir(Path::new("/w/tree"));
    rig.files.put(&ledger().join("runner-b.mb"), b"300");
    rig.disk.set(Path::new("/"), 700);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, tree_job(500)).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Failed);
    let detail = r.detail.unwrap_or_default();
    assert!(
        detail.contains("400 MB") && detail.contains("300 MB reserved"),
        "{detail}"
    );
}

#[tokio::test]
async fn running_job_holds_its_disk_reservation_and_keeps_the_machine_awake() {
    let mut rig = Rig::new();
    rig.settings.disk_ledger = Some(ledger());
    rig.files.mkdir(Path::new("/w/tree"));
    rig.disk.set(Path::new("/"), 1000);
    rig.script("job-1", JobScript::new(vec![StepScript::new(10 * SEC, 0)]));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    rig.settle().await;
    assert!(!rig.awake.held(), "idle runner lets the machine sleep");
    b.assign("job-1", 1, tree_job(500)).await;
    b.until(|m| matches!(m, lr_proto::RunnerToBroker::Started(_)))
        .await;
    rig.settle().await;
    let entry = ledger().join("runner-a.mb");
    assert_eq!(rig.files.file(&entry).as_deref(), Some(&b"500"[..]));
    assert!(rig.awake.held());
    rig.advance(10 * SEC).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Passed);
    rig.settle().await;
    assert_eq!(rig.files.file(&entry), None);
    assert!(!rig.awake.held());
    assert_eq!(rig.awake.counts(), (1, 1));
}

#[tokio::test]
async fn work_tree_job_is_measured_on_the_volume_of_its_directory() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.disk.set(Path::new("/"), 100);
    rig.disk.set(Path::new("/w"), 10_000);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, tree_job(500)).await;
    assert_eq!(result_of(&b.until_result().await).state, JobState::Passed);
    b.assign(
        "job-2",
        1,
        needing(commit_spec(vec![step("b", "make", 600)], 600), 500),
    )
    .await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Failed, "{:?}", r.detail);
    assert!(r.detail.unwrap_or_default().contains("100 MB"));
}

#[tokio::test]
async fn capabilities_report_free_disk_less_what_other_runners_reserved() {
    let mut rig = Rig::new();
    rig.settings.disk_ledger = Some(ledger());
    rig.settings.disk_mb = 5;
    rig.files.put(&ledger().join("runner-b.mb"), b"300");
    rig.files.put(&ledger().join("runner-a.mb"), b"999");
    rig.disk.set(&state_dir(), 2000);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let caps = b.hello.capabilities.clone().expect("runner capabilities");
    assert_eq!(caps.disk_mb, 1700);
    assert_eq!(
        rig.files.file(&ledger().join("runner-a.mb")),
        None,
        "a reservation left by a previous run of this runner is dropped at start"
    );
}

#[tokio::test]
async fn unmeasured_volume_falls_back_to_the_disk_found_at_start() {
    let mut rig = Rig::new();
    rig.settings.disk_mb = 42;
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    assert_eq!(b.hello.capabilities.clone().unwrap().disk_mb, 42);
}

#[tokio::test]
async fn job_runs_when_the_machine_cannot_be_kept_awake_and_says_so() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.awake.fail(Some("caffeinate missing"));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, tree_job(0)).await;
    assert_eq!(result_of(&b.until_result().await).state, JobState::Passed);
    let warned = rig.log.records().into_iter().any(|r| {
        r.level == lr_abstractions::Level::Warn
            && r.message == "keep-awake not held"
            && r.fields.iter().any(|(_, v)| v == "caffeinate missing")
    });
    assert!(warned, "{}", rig.logs_text());
}
