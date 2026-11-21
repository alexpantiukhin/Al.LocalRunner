mod common;

use common::*;
use lr_core::JobState;
use lr_proto::WireCode;
use lr_runner::WorkspaceLayout;
use std::path::{Path, PathBuf};

fn layout() -> WorkspaceLayout {
    WorkspaceLayout::new(state_dir())
}

fn commands(spec: &lr_abstractions::JobSpec) -> Vec<String> {
    spec.steps.iter().map(|s| s.spec.command.clone()).collect()
}

#[tokio::test]
async fn local_source_step_is_cd() {
    let rig = Rig::new();
    rig.files.mkdir(Path::new("/w/tree"));
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make all", 60)], 60),
    )
    .await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Passed);
    let runs = rig.process.runs();
    assert_eq!(runs.len(), 1, "nothing is cloned for a work tree");
    assert_eq!(runs[0].cwd, PathBuf::from("/w/tree"));
    assert_eq!(commands(&runs[0]), vec!["make all".to_string()]);
}

#[tokio::test]
async fn an_own_folder_job_starts_in_the_runners_own_folder_and_clones_nothing() {
    let rig = Rig::new();
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let mut spec = worktree_spec("/unused", vec![step("b", "make all", 60)], 60);
    spec.code = WireCode::Own;
    b.assign("job-1", 1, spec).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Passed);
    let runs = rig.process.runs();
    assert_eq!(
        runs.len(),
        1,
        "the steps of the order are the only commands"
    );
    assert_eq!(
        runs[0].cwd,
        layout().own_dir("acme/app", "client-1", "job-1")
    );
    assert_eq!(commands(&runs[0]), vec!["make all".to_string()]);
    assert!(
        !rig.files
            .is_dir(&layout().own_dir("acme/app", "client-1", "job-1")),
        "the folder of the job is removed when it ends"
    );
}

#[test]
fn two_jobs_of_a_project_get_different_folders() {
    let l = layout();
    assert_ne!(
        l.own_dir("acme/app", "client-1", "job-1"),
        l.own_dir("acme/app", "client-2", "job-1")
    );
    assert_ne!(
        l.own_dir("acme/app", "client-1", "job-1"),
        l.own_dir("acme/app", "client-1", "job-2")
    );
}

#[test]
fn own_folder_names_are_safe_and_distinct() {
    let l = layout();
    let a = l.own_dir("acme/app", "client-1", "job-1");
    let b = l.own_dir("acme_app", "client-1", "job-1");
    let d = l.own_dir("../../etc", "client-1", "job-1");
    assert_ne!(a, b);
    assert_eq!(
        d.parent().unwrap(),
        state_dir().join("workspace").join("own")
    );
    let name = d.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)));
}

fn job_root() -> PathBuf {
    PathBuf::from("/jobs/runner-a")
}

#[tokio::test]
async fn a_job_root_holds_workspace_cache_and_temp_while_the_state_directory_stays_empty() {
    let mut rig = Rig::new();
    rig.settings.job_root = job_root();
    rig.disk.set(Path::new("/"), 100_000);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let mut spec = own_spec(vec![step("b", "make", 600)], 600);
    spec.requirements.cache = vec!["deps".into()];
    spec.requirements.disk_mb = 1000;
    b.assign("job-1", 1, spec).await;
    assert_eq!(result_of(&b.until_result().await).state, JobState::Passed);
    let runs = rig.process.runs();
    let ws = WorkspaceLayout::new(job_root());
    let own = ws.own_dir("acme/app", "client-1", "job-1");
    assert!(own.starts_with(job_root().join("workspace")));
    assert_eq!(runs[0].cwd, own);
    let temp = job_root().join("tmp").join("lr-job-1");
    assert!(runs[0].limits.disk_dirs.contains(&temp));
    let dir = temp.to_string_lossy().into_owned();
    assert!(runs[0].env.contains(&("TEMP".into(), dir)));
    let cache = runs[0]
        .env
        .iter()
        .find(|(n, _)| *n == lr_core::cache_env_var("deps"))
        .map(|(_, v)| PathBuf::from(v))
        .expect("cache variable");
    assert!(cache.starts_with(job_root().join("cache")), "{cache:?}");
    assert!(!rig.files.is_dir(&state_dir().join("workspace")));
    assert!(!rig.files.is_dir(&state_dir().join("cache")));
    assert!(!rig.files.is_dir(&state_dir().join("tmp")));
}

#[tokio::test]
async fn the_active_job_marker_stays_in_the_state_directory_with_a_job_root() {
    let mut rig = Rig::new();
    rig.settings.job_root = job_root();
    rig.files.mkdir(Path::new("/w/tree"));
    rig.script(
        "job-1",
        lr_abstractions::fakes::JobScript::new(vec![lr_abstractions::fakes::StepScript::new(
            std::time::Duration::from_secs(600),
            0,
        )]),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("b", "make", 600)], 600),
    )
    .await;
    b.next_job_msg().await.unwrap();
    rig.settle().await;
    assert!(rig.files.file(&state_dir().join("active-job")).is_some());
    assert!(rig.files.file(&job_root().join("active-job")).is_none());
}
