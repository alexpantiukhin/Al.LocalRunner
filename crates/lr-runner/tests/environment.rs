mod common;

use common::*;
use lr_abstractions::fakes::{JobScript, StepScript};
use lr_abstractions::{CacheEntry, CacheKey, RunnerStore};
use lr_core::{cache_env_var, JobState, Timestamp};
use lr_proto::{ReasonCode, RunnerToBroker};
use lr_runner::{plan_sweep, CacheLayout, Retention};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

const DAY: u64 = 86_400_000;
const GB: u64 = 1024 * 1024 * 1024;

fn env_of(spec: &lr_abstractions::JobSpec) -> Vec<(String, String)> {
    spec.env.clone()
}

#[tokio::test]
async fn job_environment_is_built_from_scratch() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 60)], 60),
    )
    .await;
    b.until_result().await;
    let env = env_of(&rig.process.runs()[0]);
    let names: BTreeSet<&str> = env.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        BTreeSet::from(["PATH", "HOME", "TEMP", "LOCALRUNNER_JOB_ID"])
    );
    assert!(env.contains(&("PATH".into(), "/usr/bin:/bin".into())));
    assert!(env.contains(&("LOCALRUNNER_JOB_ID".into(), "job-1".into())));
}

#[tokio::test]
async fn secrets_reach_only_the_steps_that_requested_them() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.secrets.insert("signing_key", "s3cr3t-value");
    rig.secrets.insert("registry_auth", "tok-123");
    rig.script(
        "job-1",
        JobScript::new(vec![StepScript::new(Duration::ZERO, 0).output(b"done\n")]),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let mut publish = step("publish", "sign", 60);
    publish.secrets = vec!["signing_key".into()];
    let mut spec = worktree_spec("/w/tree", vec![step("build", "make", 60), publish], 60);
    spec.secrets = vec!["signing_key".into(), "registry_auth".into()];
    b.assign("job-1", 1, spec).await;
    assert_eq!(result_of(&b.until_result().await).state, JobState::Passed);
    let run = &rig.process.runs()[0];
    let names = |i: usize| -> Vec<String> {
        run.steps[i]
            .secrets
            .iter()
            .map(|(n, _)| n.clone())
            .collect()
    };
    assert_eq!(names(0), vec!["REGISTRY_AUTH".to_string()]);
    assert_eq!(
        names(1),
        vec!["SIGNING_KEY".to_string(), "REGISTRY_AUTH".to_string()]
    );
    assert_eq!(run.steps[1].secrets[0].1.expose(), "s3cr3t-value");
    assert!(run
        .env
        .iter()
        .all(|(_, v)| v != "s3cr3t-value" && v != "tok-123"));
    let logs = rig.logs_text();
    assert!(!logs.contains("s3cr3t-value") && !logs.contains("tok-123"));
}

#[tokio::test]
async fn unavailable_secret_ends_the_job_with_secret_missing() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let mut spec = worktree_spec("/w/tree", vec![step("b", "make", 60)], 60);
    spec.secrets = vec!["signing_key".into()];
    b.assign("job-1", 1, spec).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Failed);
    assert_eq!(r.reason, Some(ReasonCode::SecretMissing));
    assert_eq!(r.detail.as_deref(), Some("secret not found: signing_key"));
    assert!(
        rig.process.runs().is_empty(),
        "never run without the secret"
    );
}

#[tokio::test]
async fn named_cache_is_a_persistent_directory_passed_by_variable() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let mut spec = worktree_spec("/w/tree", vec![step("b", "make", 60)], 60);
    spec.requirements.cache = vec!["deps".into()];
    b.assign("job-1", 1, spec).await;
    b.until_result().await;
    let key = CacheKey {
        client: "client-1".into(),
        project: "acme/app".into(),
        name: "deps".into(),
    };
    let path = CacheLayout::new(state_dir()).path(&key);
    assert!(path.starts_with(state_dir().join("cache")));
    let env = env_of(&rig.process.runs()[0]);
    assert!(env.contains(&(cache_env_var("deps"), path.display().to_string())));
    assert_eq!(cache_env_var("deps"), "LOCALRUNNER_CACHE_DEPS");
    assert!(rig.files.is_dir(&path));
    rig.settle().await;
    let entries = rig.store.cache_entries().await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, key);
    assert_eq!(entries[0].last_used, Timestamp(START));
}

#[tokio::test]
async fn caches_of_two_clients_never_share_a_directory() {
    let l = CacheLayout::new(state_dir());
    let k = |client: &str| CacheKey {
        client: client.into(),
        project: "acme/app".into(),
        name: "deps".into(),
    };
    assert_ne!(l.path(&k("client-1")), l.path(&k("client-2")));
}

#[tokio::test]
async fn runner_reports_cache_limit_and_fill() {
    let mut rig = Rig::new();
    rig.settings.cache = Retention {
        max_size_gb: Some(40),
        retention_days: Some(14),
    };
    let key = CacheKey {
        client: "c".into(),
        project: "p".into(),
        name: "n".into(),
    };
    let path = CacheLayout::new(state_dir()).path(&key);
    rig.files
        .put(&path.join("blob"), &vec![0u8; 3 * 1024 * 1024]);
    rig.store
        .upsert_cache(CacheEntry {
            key,
            path: path.display().to_string(),
            bytes: 3 * 1024 * 1024,
            last_used: Timestamp(START),
        })
        .await
        .unwrap();
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let caps = b.hello.capabilities.clone().unwrap();
    assert_eq!(caps.cache_max_mb, Some(40 * 1024));
    assert_eq!(caps.cache_used_mb, Some(3));
}

#[tokio::test]
async fn cache_fill_is_reported_after_a_job() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let mut spec = worktree_spec("/w/tree", vec![step("b", "make", 60)], 60);
    spec.requirements.cache = vec!["deps".into()];
    b.assign("job-1", 1, spec).await;
    b.until_result().await;
    let m = b
        .until(|m| matches!(m, RunnerToBroker::CapabilitiesUpdate(_)))
        .await;
    let RunnerToBroker::CapabilitiesUpdate(u) = m else {
        unreachable!()
    };
    assert_eq!(u.capabilities.cache_used_mb, Some(0));
}

fn entry(name: &str, bytes: u64, last_used: u64) -> CacheEntry {
    CacheEntry {
        key: CacheKey {
            client: "c".into(),
            project: "p".into(),
            name: name.into(),
        },
        path: format!("/c/{name}"),
        bytes,
        last_used: Timestamp(last_used),
    }
}

#[test]
fn sweep_drops_entries_unused_longer_than_retention() {
    let now = 100 * DAY;
    let entries = vec![
        entry("old", 1, now - 15 * DAY),
        entry("fresh", 1, now - 13 * DAY),
    ];
    let r = Retention {
        max_size_gb: None,
        retention_days: Some(14),
    };
    let doomed = plan_sweep(&entries, Timestamp(now), &r, &[]);
    assert_eq!(
        doomed.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(),
        ["old"]
    );
}

#[test]
fn sweep_evicts_least_recently_used_down_to_the_limit() {
    let now = 100 * DAY;
    let entries = vec![
        entry("a", GB, now - 3 * DAY),
        entry("b", GB, now - 2 * DAY),
        entry("c", GB, now - DAY),
    ];
    let r = Retention {
        max_size_gb: Some(2),
        retention_days: None,
    };
    let doomed = plan_sweep(&entries, Timestamp(now), &r, &[]);
    assert_eq!(
        doomed.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(),
        ["a"]
    );
}

#[test]
fn sweep_never_removes_a_cache_in_use() {
    let now = 100 * DAY;
    let entries = vec![entry("a", 3 * GB, now - 30 * DAY), entry("b", GB, now)];
    let r = Retention {
        max_size_gb: Some(1),
        retention_days: Some(14),
    };
    let in_use = [entries[0].key.clone()];
    let doomed = plan_sweep(&entries, Timestamp(now), &r, &in_use);
    assert_eq!(
        doomed.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(),
        ["b"]
    );
}

#[tokio::test]
async fn stale_cache_is_removed_before_a_job() {
    let mut rig = Rig::new();
    rig.settings.cache = Retention {
        max_size_gb: None,
        retention_days: Some(14),
    };
    rig.files.mkdir(Path::new("/w/tree"));
    let old = entry("old", 5, START - 15 * DAY);
    rig.files.put(&Path::new(&old.path).join("f"), b"12345");
    rig.store.upsert_cache(old.clone()).await.unwrap();
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 60)], 60),
    )
    .await;
    b.until_result().await;
    assert!(rig.store.cache_entries().await.unwrap().is_empty());
    assert!(!rig.files.is_dir(Path::new(&old.path)));
}

#[tokio::test]
async fn idle_runner_sweeps_the_cache_every_hour() {
    let mut rig = Rig::new();
    rig.settings.cache = Retention {
        max_size_gb: None,
        retention_days: Some(14),
    };
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let old = entry("old", 5, START - 15 * DAY);
    rig.store.upsert_cache(old.clone()).await.unwrap();
    for _ in 0..3599 {
        rig.clock.advance(Duration::from_secs(1));
    }
    rig.settle().await;
    assert_eq!(rig.store.cache_entries().await.unwrap().len(), 1);
    rig.advance(Duration::from_secs(1)).await;
    assert!(rig.store.cache_entries().await.unwrap().is_empty());
    drop(b);
}

#[tokio::test]
async fn runner_without_env_path_refuses_the_job_instead_of_running_it_without_path() {
    let mut rig = Rig::new();
    rig.settings.path = None;
    rig.files.mkdir(Path::new("/w/tree"));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 60)], 60),
    )
    .await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Failed);
    assert_eq!(
        r.detail.as_deref(),
        Some("runner.toml has no [env] path, so the job would get no PATH; set [env] path (spec 13.4)")
    );
    assert!(rig.process.runs().is_empty());
}
