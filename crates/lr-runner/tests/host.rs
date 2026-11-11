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

struct Yielding(lr_abstractions::fakes::MemoryFiles);

#[async_trait::async_trait]
impl lr_abstractions::Files for Yielding {
    async fn exists(&self, p: &Path) -> Result<bool, lr_abstractions::FilesError> {
        tokio::task::yield_now().await;
        self.0.exists(p).await
    }
    async fn create_dir_all(&self, p: &Path) -> Result<(), lr_abstractions::FilesError> {
        tokio::task::yield_now().await;
        self.0.create_dir_all(p).await
    }
    async fn remove_dir_all(&self, p: &Path) -> Result<(), lr_abstractions::FilesError> {
        tokio::task::yield_now().await;
        self.0.remove_dir_all(p).await
    }
    async fn size(&self, p: &Path) -> Result<u64, lr_abstractions::FilesError> {
        tokio::task::yield_now().await;
        self.0.size(p).await
    }
    async fn read(&self, p: &Path) -> Result<Option<Vec<u8>>, lr_abstractions::FilesError> {
        tokio::task::yield_now().await;
        self.0.read(p).await
    }
    async fn write(&self, p: &Path, d: &[u8]) -> Result<(), lr_abstractions::FilesError> {
        tokio::task::yield_now().await;
        self.0.write(p, d).await
    }
    async fn remove_file(&self, p: &Path) -> Result<(), lr_abstractions::FilesError> {
        tokio::task::yield_now().await;
        self.0.remove_file(p).await
    }
    async fn list_files(&self, p: &Path) -> Result<Vec<String>, lr_abstractions::FilesError> {
        tokio::task::yield_now().await;
        self.0.list_files(p).await
    }
}

#[tokio::test]
async fn two_runners_never_both_take_the_same_free_disk() {
    let mut a = Rig::new();
    let mut b = Rig::new();
    b.files = a.files.clone();
    b.settings.name = "runner-b".into();
    b.settings.state_dir = PathBuf::from("/state/runner-b");
    for rig in [&mut a, &mut b] {
        rig.settings.disk_ledger = Some(ledger());
        rig.disk.set(Path::new("/"), 1000);
        rig.script("job-1", JobScript::new(vec![StepScript::new(10 * SEC, 0)]));
    }
    a.files.mkdir(Path::new("/w/tree"));
    let start = |rig: &Rig| {
        let mut deps = rig.deps();
        deps.files = std::sync::Arc::new(Yielding(rig.files.clone()));
        tokio::spawn(lr_runner::RunnerAgent::new(deps).run())
    };
    let (_ha, _hb) = (start(&a), start(&b));
    let (ba, bb) = tokio::join!(Broker::accept(&a), Broker::accept(&b));
    tokio::join!(
        ba.assign("job-1", 1, tree_job(600)),
        bb.assign("job-1", 1, tree_job(600))
    );
    for _ in 0..20 {
        a.settle().await;
    }
    let running = a.process.runs().len() + b.process.runs().len();
    assert!(running <= 1, "both runners took 600 of 1000 MB");
}

#[tokio::test]
async fn reservation_of_a_dead_runner_process_is_dropped() {
    let mut rig = Rig::new();
    rig.settings.disk_ledger = Some(ledger());
    rig.files.mkdir(Path::new("/w/tree"));
    rig.process.processes().add(lr_abstractions::ProcessRef {
        pid: 777,
        start: Some(5),
    });
    rig.files.put(&ledger().join("runner-b.mb"), b"300 777 5");
    rig.files.put(&ledger().join("runner-c.mb"), b"600 888 9");
    rig.disk.set(Path::new("/"), 1000);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, tree_job(600)).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Passed, "{:?}", r.detail);
    assert_eq!(rig.files.file(&ledger().join("runner-c.mb")), None);
    assert!(rig.files.file(&ledger().join("runner-b.mb")).is_some());
}

#[tokio::test]
async fn own_reservation_names_this_runner_process() {
    let mut rig = Rig::new();
    rig.settings.disk_ledger = Some(ledger());
    rig.settings.process = Some(lr_abstractions::ProcessRef {
        pid: 4242,
        start: Some(99),
    });
    rig.files.mkdir(Path::new("/w/tree"));
    rig.disk.set(Path::new("/"), 1000);
    rig.script("job-1", JobScript::new(vec![StepScript::new(10 * SEC, 0)]));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, tree_job(500)).await;
    b.until(|m| matches!(m, lr_proto::RunnerToBroker::Started(_)))
        .await;
    rig.settle().await;
    assert_eq!(
        rig.files.file(&ledger().join("runner-a.mb")).as_deref(),
        Some(&b"500 4242 99"[..])
    );
}

#[tokio::test]
async fn unreadable_reservation_refuses_rather_than_counting_zero() {
    let mut rig = Rig::new();
    rig.settings.disk_ledger = Some(ledger());
    rig.files.mkdir(Path::new("/w/tree"));
    rig.files.put(&ledger().join("runner-b.mb"), b"lots");
    rig.disk.set(Path::new("/"), 100_000);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, tree_job(10)).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Failed);
    assert_eq!(r.reason, Some(lr_proto::ReasonCode::DiskInsufficient));
    assert!(
        r.detail.unwrap_or_default().contains("runner-b.mb"),
        "the refusal names the unreadable reservation"
    );
    assert_eq!(rig.files.file(&ledger().join("runner-a.mb")), None);
}
