mod common;

use common::*;
use lr_abstractions::fakes::{JobScript, StepScript};
use lr_abstractions::{Enforce, Exceeded, LimitedResource, Usage};
use lr_core::JobState;
use lr_proto::{AssignSpec, ReasonCode, WireEnforce, WireLimits};
use lr_runner::{CleanupOn, RunnerSettings};
use std::path::{Path, PathBuf};
use std::time::Duration;

const SEC: Duration = Duration::from_secs(1);

fn parse(text: &str) -> Result<RunnerSettings, lr_runner::SettingsError> {
    RunnerSettings::from_toml(&format!("name = \"r\"\n{text}"), PathBuf::from("/s"))
}

fn refused(text: &str) -> String {
    parse(text).unwrap_err().0
}

fn job(mem: u64, disk: u64) -> AssignSpec {
    let mut spec = worktree_spec("/w/tree", vec![step("b", "make", 600)], 600);
    spec.requirements.mem_mb = mem;
    spec.requirements.cpu = 2;
    spec.requirements.disk_mb = disk;
    spec
}

fn kill_policy() -> WireLimits {
    WireLimits {
        mode: WireEnforce::Kill,
        mem_headroom_pct: 125,
        disk_headroom_pct: 150,
    }
}

fn hook(name: &str, command: &str) -> String {
    format!("[[cleanup]]\nname = \"{name}\"\ncommand = \"{command}\"\n")
}

#[test]
fn limits_keys_are_parsed() {
    let s = parse("[limits]\nenforce = \"kill\"\nmem_headroom = 1.5\ndisk_headroom = 2\n").unwrap();
    assert_eq!(s.enforce, Some(Enforce::Kill));
    assert_eq!(
        (s.mem_headroom_pct, s.disk_headroom_pct),
        (Some(150), Some(200))
    );
    let none = parse("").unwrap();
    assert_eq!(
        (none.enforce, none.mem_headroom_pct, none.disk_headroom_pct),
        (None, None, None)
    );
    assert!(none.cleanup.is_empty() && none.job_env.is_empty());
}

#[test]
fn bad_limits_keys_are_refused() {
    assert!(refused("[limits]\nenforce = \"stop\"\n").contains("enforce"));
    assert!(refused("[limits]\nmem_headroom = 0.5\n").contains("mem_headroom"));
    assert!(refused("[limits]\ndisk_headroom = 1000.0\n").contains("disk_headroom"));
}

#[test]
fn cleanup_entries_get_defaults_and_are_validated() {
    let s = parse(&hook("docker", "stop-containers")).unwrap();
    let h = &s.cleanup[0];
    assert_eq!((h.timeout_s, h.on.len()), (60, 5));
    let s = parse("[[cleanup]]\nname = \"a\"\ncommand = \"x\"\ntimeout_s = 5\non = [\"killed\", \"timeout\"]\n").unwrap();
    assert_eq!(s.cleanup[0].timeout_s, 5);
    assert!(
        s.cleanup[0].on.contains(&CleanupOn::Killed)
            && !s.cleanup[0].on.contains(&CleanupOn::Passed)
    );
    assert!(refused(&format!("{}{}", hook("a", "x"), hook("a", "y"))).contains("twice"));
    assert!(refused(&hook("a", " ")).contains("empty command"));
    assert!(
        refused("[[cleanup]]\nname = \"a\"\ncommand = \"x\"\ntimeout_s = 0\n")
            .contains("timeout_s")
    );
    assert!(
        refused("[[cleanup]]\nname = \"a\"\ncommand = \"x\"\non = [\"never\"]\n").contains("never")
    );
    assert!(refused("[[cleanup]]\nname = \"a\"\ncommand = \"x\"\non = []\n").contains("`on`"));
}

#[test]
fn job_env_is_validated() {
    let s = parse("[job_env]\nGRADLE_OPTS = \"-Dorg.gradle.daemon=false\"\n").unwrap();
    assert_eq!(s.job_env["GRADLE_OPTS"], "-Dorg.gradle.daemon=false");
    for bad in ["PATH", "path", "HOME", "LOCALRUNNER_JOB_ID", "1X", "A-B"] {
        let text = format!("[job_env]\n\"{bad}\" = \"v\"\n");
        assert!(refused(&text).contains("[job_env]"), "{bad}");
    }
}

#[test]
fn cleanup_outcome_follows_state_and_reason() {
    use JobState::*;
    let on = |s, r: Option<ReasonCode>| CleanupOn::of(s, r.as_ref());
    assert_eq!(on(Passed, None), CleanupOn::Passed);
    assert_eq!(on(Failed, None), CleanupOn::Failed);
    assert_eq!(
        on(Failed, Some(ReasonCode::ResourceExceeded)),
        CleanupOn::Killed
    );
    assert_eq!(
        on(Cancelled, Some(ReasonCode::BrokerUnavailable)),
        CleanupOn::Killed
    );
    assert_eq!(
        on(Cancelled, Some(ReasonCode::CancelledByUser)),
        CleanupOn::Cancelled
    );
    assert_eq!(on(Timeout, None), CleanupOn::Timeout);
}

#[test]
fn owner_env_wins_over_existing_variables() {
    let mut env = vec![
        ("A".to_string(), "1".to_string()),
        ("B".to_string(), "2".to_string()),
    ];
    let owner = [
        ("B".to_string(), "9".to_string()),
        ("C".to_string(), "3".to_string()),
    ]
    .into();
    lr_runner::apply_owner_env(&mut env, &owner);
    assert_eq!(env.iter().filter(|(n, _)| n == "B").count(), 1);
    assert!(env.contains(&("B".into(), "9".into())) && env.contains(&("C".into(), "3".into())));
}

#[tokio::test]
async fn limits_reach_the_process_runner_and_peaks_are_reported() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.disk.set(Path::new("/"), 100_000);
    rig.script(
        "job-1",
        JobScript::new(vec![StepScript::new(Duration::ZERO, 0)]).usage(Usage {
            peak_mem_mb: Some(3000),
            peak_disk_mb: Some(700),
        }),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let mut spec = job(4000, 1000);
    spec.limits = Some(kill_policy());
    b.assign("job-1", 1, spec).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Passed);
    assert_eq!((r.peak_mem_mb, r.peak_disk_mb), (Some(3000), Some(700)));
    let l = &rig.process.runs()[0].limits;
    assert_eq!(l.mode, Enforce::Kill);
    assert_eq!(
        (l.mem_mb, l.disk_mb, l.cpu_millis),
        (Some(5000), Some(1500), Some(2000))
    );
    assert_eq!(l.procs, Some(512));
}

#[tokio::test]
async fn with_enforce_off_nothing_is_limited() {
    let mut rig = Rig::new();
    rig.settings.enforce = Some(Enforce::Off);
    rig.files.mkdir(Path::new("/w/tree"));
    rig.disk.set(Path::new("/"), 100_000);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, job(4000, 1000)).await;
    assert_eq!(result_of(&b.until_result().await).state, JobState::Passed);
    let l = &rig.process.runs()[0].limits;
    assert_eq!(l.mode, Enforce::Off);
    assert_eq!(
        (l.mem_mb, l.disk_mb, l.cpu_millis, l.procs),
        (None, None, None, None)
    );
}

#[tokio::test]
async fn runner_toml_mode_replaces_the_broker_mode() {
    let mut rig = Rig::new();
    rig.settings.enforce = Some(Enforce::Off);
    rig.files.mkdir(Path::new("/w/tree"));
    rig.disk.set(Path::new("/"), 100_000);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let mut spec = job(4000, 0);
    spec.limits = Some(kill_policy());
    b.assign("job-1", 1, spec).await;
    b.until_result().await;
    assert_eq!(rig.process.runs()[0].limits.mode, Enforce::Off);
}

#[tokio::test]
async fn exceeded_job_fails_with_the_resource_reason() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script(
        "job-1",
        JobScript::new(vec![StepScript::new(Duration::ZERO, 0)])
            .usage(Usage {
                peak_mem_mb: Some(5200),
                peak_disk_mb: None,
            })
            .exceeded(Exceeded {
                resource: LimitedResource::Memory,
                limit_mb: 5000,
                declared_mb: Some(4000),
                peak_mb: 5200,
            }),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, job(4000, 0)).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Failed);
    assert_eq!(r.reason, Some(ReasonCode::ResourceExceeded));
    assert_eq!(
        r.detail.as_deref(),
        Some("превышен заявленный mem_mb: заявлено 4000 МБ, пик 5200 МБ")
    );
    assert_eq!(r.peak_mem_mb, Some(5200));
}

#[tokio::test]
async fn disk_admission_requires_the_headroom() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.disk.set(Path::new("/"), 1200);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, job(0, 1000)).await;
    assert_eq!(result_of(&b.until_result().await).state, JobState::Passed);
    let mut spec = job(0, 1000);
    spec.limits = Some(kill_policy());
    b.assign("job-2", 1, spec).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.reason, Some(ReasonCode::DiskInsufficient));
    assert!(r.detail.unwrap().contains("needs 1500 MB"));
}

fn with_hooks(rig: &mut Rig, hooks: &str) {
    let text = format!("name = \"runner-a\"\n{hooks}");
    let parsed = RunnerSettings::from_toml(&text, rig.settings.state_dir.clone()).unwrap();
    rig.settings.cleanup = parsed.cleanup;
    rig.settings.job_env = parsed.job_env;
    rig.files.mkdir(Path::new("/w/tree"));
}

fn hook_runs(rig: &Rig) -> Vec<lr_abstractions::JobSpec> {
    rig.process
        .runs()
        .into_iter()
        .filter(|r| r.job.0.contains(".cleanup."))
        .collect()
}

#[tokio::test]
async fn cleanup_runs_after_pass_and_fail_with_the_job_label() {
    for (code, expect) in [(0, JobState::Passed), (3, JobState::Failed)] {
        let mut rig = Rig::new();
        with_hooks(
            &mut rig,
            &format!(
                "{}[job_env]\nGRADLE_OPTS = \"-Dx\"\n",
                hook("docker", "remove-labelled")
            ),
        );
        rig.script(
            "job-1",
            JobScript::new(vec![StepScript::new(Duration::ZERO, code)]),
        );
        let (_c, _h) = rig.start();
        let b = Broker::accept(&rig).await;
        b.assign("job-1", 1, job(0, 0)).await;
        let msgs = b.until_result().await;
        assert_eq!(result_of(&msgs).state, expect);
        let runs = hook_runs(&rig);
        assert_eq!(runs.len(), 1);
        let h = &runs[0];
        assert_eq!(h.steps[0].spec.command, "remove-labelled");
        assert_eq!(h.timeout_s, 60);
        assert_eq!(
            h.header,
            vec![("cleanup".to_string(), "docker".to_string())]
        );
        assert!(h
            .env
            .contains(&("LOCALRUNNER_JOB_LABEL".into(), "lr-job-1".into())));
        assert!(h
            .env
            .contains(&("LOCALRUNNER_JOB_ID".into(), "job-1".into())));
        assert!(h.env.contains(&("GRADLE_OPTS".into(), "-Dx".into())));
        let job_run = &rig.process.runs()[0];
        assert!(job_run.env.contains(&("GRADLE_OPTS".into(), "-Dx".into())));
        assert!(job_run
            .env
            .contains(&("LOCALRUNNER_JOB_LABEL".into(), "lr-job-1".into())));
    }
}

#[tokio::test]
async fn failing_hook_is_logged_and_does_not_change_the_result() {
    let mut rig = Rig::new();
    with_hooks(&mut rig, &hook("flaky", "boom"));
    rig.script(
        "job-1.cleanup.flaky",
        JobScript::new(vec![StepScript::new(Duration::ZERO, 7)]),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, job(0, 0)).await;
    let msgs = b.until_result().await;
    assert_eq!(result_of(&msgs).state, JobState::Passed);
    assert!(chunks_text(&msgs).contains("warning: cleanup flaky ended failed (exit code 7)"));
    assert!(rig.logs_text().contains("cleanup hook failed"));
}

#[tokio::test]
async fn hooks_follow_their_on_list() {
    let mut rig = Rig::new();
    with_hooks(
        &mut rig,
        "[[cleanup]]\nname = \"only-failed\"\ncommand = \"x\"\non = [\"failed\"]\n",
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, job(0, 0)).await;
    b.until_result().await;
    assert!(hook_runs(&rig).is_empty());
}

#[tokio::test]
async fn cleanup_runs_after_cancel() {
    let mut rig = Rig::new();
    with_hooks(&mut rig, &hook("any", "x"));
    rig.script("job-1", JobScript::new(vec![StepScript::new(600 * SEC, 0)]));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, job(0, 0)).await;
    b.next_job_msg().await.unwrap();
    b.cancel("job-1", 2, ReasonCode::CancelledByUser).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Cancelled);
    assert_eq!(hook_runs(&rig).len(), 1);
}

#[tokio::test]
async fn cleanup_runs_after_a_resource_kill_and_a_timeout() {
    let mut rig = Rig::new();
    with_hooks(
        &mut rig,
        "[[cleanup]]\nname = \"k\"\ncommand = \"x\"\non = [\"killed\"]\n[[cleanup]]\nname = \"t\"\ncommand = \"x\"\non = [\"timeout\"]\n",
    );
    rig.script(
        "job-1",
        JobScript::new(vec![StepScript::new(Duration::ZERO, 0)]).exceeded(Exceeded {
            resource: LimitedResource::Disk,
            limit_mb: 10,
            declared_mb: Some(8),
            peak_mb: 11,
        }),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, job(0, 0)).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.reason, Some(ReasonCode::ResourceExceeded));
    let names: Vec<String> = hook_runs(&rig).iter().map(|r| r.job.0.clone()).collect();
    assert_eq!(names, vec!["job-1.cleanup.k".to_string()]);
}
