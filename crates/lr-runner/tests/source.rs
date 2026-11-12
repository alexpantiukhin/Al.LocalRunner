mod common;

use common::*;
use lr_core::{JobId, JobState};
use lr_proto::{RunnerToBroker, WireCode};
use lr_runner::{quote_arg, validate_branch, validate_commit, validate_repo_url, WorkspaceLayout};
use std::path::{Path, PathBuf};

const URL: &str = "https://git.example.com/acme/app.git";
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn layout() -> WorkspaceLayout {
    WorkspaceLayout::new(state_dir())
}

fn q(p: &Path) -> String {
    quote_arg(&p.display().to_string()).unwrap()
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
async fn remote_source_creates_mirror_when_absent() {
    let rig = Rig::new();
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, commit_spec(vec![step("b", "make", 60)], 60))
        .await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Passed);
    let l = layout();
    let ws = l.project_dir("acme/app", URL);
    let mirror = l.mirror_dir("acme/app", URL);
    let job_dir = l.job_dir("acme/app", URL, "client-1", &JobId("job-1".into()));
    assert!(ws.starts_with(state_dir().join("workspace")));
    assert!(ws
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("acme_app-"));
    assert!(rig.files.is_dir(&ws), "mkdir of the workspace");
    let runs = rig.process.runs();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].cwd, ws);
    assert_eq!(
        commands(&runs[0]),
        vec![
            format!("git clone --mirror -- {} {}", q(Path::new(URL)), q(&mirror)),
            format!("git -C {} fetch --prune -- origin", q(&mirror)),
            format!("git -C {} worktree prune", q(&mirror)),
            format!(
                "git -C {} worktree add --detach -- {} {SHA}",
                q(&mirror),
                q(&job_dir)
            ),
        ]
    );
    assert_eq!(runs[0].timeout_s, 60);
    assert_eq!(runs[1].cwd, job_dir);
    assert_eq!(commands(&runs[1]), vec!["make".to_string()]);
    assert!(
        !rig.files.is_dir(&job_dir),
        "the checkout is removed after the job"
    );
}

#[tokio::test]
async fn existing_mirror_is_only_fetched() {
    let rig = Rig::new();
    let mirror = layout().mirror_dir("acme/app", URL);
    rig.files.mkdir(&mirror);
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, commit_spec(vec![step("b", "make", 60)], 60))
        .await;
    b.until_result().await;
    let cmds = commands(&rig.process.runs()[0]);
    assert_eq!(cmds.len(), 3);
    assert!(cmds[0].starts_with("git -C ") && cmds[0].ends_with(" fetch --prune -- origin"));
    assert!(cmds.iter().all(|c| !c.contains("clone")));
}

#[tokio::test]
async fn failed_source_step_fails_the_job_and_skips_profile_steps() {
    let rig = Rig::new();
    rig.script(
        "job-1",
        lr_abstractions::fakes::JobScript::new(vec![lr_abstractions::fakes::StepScript::new(
            std::time::Duration::ZERO,
            128,
        )]),
    );
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign("job-1", 1, commit_spec(vec![step("b", "make", 60)], 60))
        .await;
    let msgs = b.until_result().await;
    let r = result_of(&msgs);
    assert_eq!(r.state, JobState::Failed);
    assert_eq!(r.detail.as_deref(), Some("source step failed: clone"));
    assert_eq!(rig.process.runs().len(), 1);
    assert!(msgs.iter().any(|m| matches!(m,
        RunnerToBroker::StepResult(s) if s.ord == 0 && s.state == lr_core::StepState::Skipped)));
}

#[tokio::test]
async fn profile_source_overrides_default() {
    let rig = Rig::new();
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let steps = vec![
        step(
            "source",
            "fetch-code {repo_url} {commit} {branch} {workspace} {job_dir}",
            60,
        ),
        step("build", "make", 60),
    ];
    b.assign("job-1", 1, commit_spec(steps, 60)).await;
    let msgs = b.until_result().await;
    assert_eq!(result_of(&msgs).state, JobState::Passed);
    let l = layout();
    let ws = l.project_dir("acme/app", URL);
    let job_dir = l.job_dir("acme/app", URL, "client-1", &JobId("job-1".into()));
    let runs = rig.process.runs();
    assert_eq!(runs.len(), 2);
    assert_eq!(
        commands(&runs[0]),
        vec![format!(
            "fetch-code {} {SHA} {} {} {}",
            q(Path::new(URL)),
            quote_arg("main").unwrap(),
            q(&ws),
            q(&job_dir)
        )]
    );
    assert_eq!(runs[0].cwd, ws);
    assert_eq!(commands(&runs[1]), vec!["make".to_string()]);
    let ords: Vec<(u32, String)> = msgs
        .iter()
        .filter_map(|m| match m {
            RunnerToBroker::StepResult(s) => Some((s.ord, s.name.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(ords, vec![(0, "source".into()), (1, "build".into())]);
}

#[tokio::test]
async fn commit_without_repo_url_fails() {
    let rig = Rig::new();
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let mut spec = commit_spec(vec![step("b", "make", 60)], 60);
    spec.code = WireCode::Commit {
        sha: SHA.into(),
        repo_url: None,
        branch: None,
    };
    b.assign("job-1", 1, spec).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Failed);
    assert_eq!(
        r.detail.as_deref(),
        Some("source: the order has no repository address")
    );
    assert!(rig.process.runs().is_empty());
}

#[tokio::test]
async fn hostile_repo_url_never_reaches_a_command() {
    let rig = Rig::new();
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let mut spec = commit_spec(vec![step("b", "make", 60)], 60);
    spec.code = WireCode::Commit {
        sha: SHA.into(),
        repo_url: Some("https://x/\"; rm -rf ~; \"".into()),
        branch: None,
    };
    b.assign("job-1", 1, spec).await;
    let r = result_of(&b.until_result().await);
    assert_eq!(r.state, JobState::Failed);
    assert!(rig.process.runs().is_empty());
}

#[test]
fn repo_urls_in_common_forms_are_accepted() {
    for url in [
        "https://github.com/acme/app.git",
        "https://github.com/acme/app",
        "http://git.local:8080/acme/app.git",
        "ssh://git@github.com/acme/app.git",
        "ssh://git@github.com:22/acme/app",
        "git@github.com:acme/app.git",
        "git@github.com:acme/app",
        "git://git.local/acme/app.git",
        "file:///srv/git/app.git",
    ] {
        assert!(validate_repo_url(url).is_ok(), "{url}");
    }
}

#[test]
fn dangerous_repo_urls_are_refused() {
    for url in [
        "",
        "-uhttps://x/y",
        "--upload-pack=touch /tmp/p",
        "ext::sh -c touch% /tmp/pwned",
        "fd::17",
        "git@-oProxyCommand=x:y",
        "ssh://-oProxyCommand=x/y",
        "https://x/y;id",
        "https://x/$(id)",
        "https://x/`id`",
        "https://x/a b",
        "https://x/%41",
        "https://x/a\"b",
        "https://x/a\nb",
        "C:/repos/app",
        "/srv/git/app",
        "ftp://x/y",
        "https://",
    ] {
        assert!(validate_repo_url(url).is_err(), "{url:?}");
    }
    assert!(validate_repo_url(&format!("https://x/{}", "a".repeat(2048))).is_err());
}

#[test]
fn commits_must_be_hex_object_names() {
    assert!(validate_commit(SHA).is_ok());
    assert!(validate_commit("abc1234").is_ok());
    assert!(validate_commit(&"a".repeat(64)).is_ok());
    for bad in [
        "abc123",
        "-abcdef1",
        "HEAD",
        "main",
        "abcdefg",
        &"a".repeat(65),
    ] {
        assert!(validate_commit(bad).is_err(), "{bad}");
    }
}

#[test]
fn branch_names_follow_ref_rules() {
    for ok in ["main", "feature/x-1", "release/1.2"] {
        assert!(validate_branch(ok).is_ok(), "{ok}");
    }
    for bad in [
        "", "-x", "/x", "a..b", "a//b", "x.lock", "x/", "x.", "a b", "a;b", "a$b",
    ] {
        assert!(validate_branch(bad).is_err(), "{bad}");
    }
}

#[test]
fn quoting_refuses_shell_specials() {
    assert_eq!(quote_arg("/w/a b").unwrap(), "\"/w/a b\"");
    assert_eq!(quote_arg("/w/проект").unwrap(), "\"/w/проект\"");
    for bad in [
        "a\"b", "a$b", "a`b", "a%b", "a!b", "a^b", "a\nb", "a\u{7}b", "a\\",
    ] {
        assert!(quote_arg(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn workspace_names_are_safe_and_distinct() {
    let l = layout();
    let a = l.project_dir("acme/app", URL);
    let b = l.project_dir("acme_app", URL);
    let c = l.project_dir("acme/app", "https://other.example/acme/app.git");
    let d = l.project_dir("../../etc", URL);
    assert_ne!(a, b);
    assert_ne!(a, c, "another repository gets another mirror");
    assert_eq!(d.parent().unwrap(), state_dir().join("workspace"));
    let name = d.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)));
    assert_eq!(l.mirror_dir("acme/app", URL), a.join("mirror.git"));
    let j = l.job_dir("acme/app", URL, "client-1", &JobId("a/../b".into()));
    assert_eq!(j.parent().unwrap(), a.join("jobs"));
}

#[tokio::test]
async fn checkouts_left_by_a_crash_are_removed_at_start_and_the_mirror_stays() {
    use lr_abstractions::{CacheEntry, CacheKey, RunnerStore};
    let rig = Rig::new();
    let mirror = layout().mirror_dir("acme/app", URL);
    let left = layout().job_dir("acme/app", URL, "client-1", &JobId("job-0".into()));
    rig.files.mkdir(&mirror);
    rig.files.put(&left.join("Cargo.toml"), b"x");
    let ws = layout().project_dir("acme/app", URL);
    rig.store
        .upsert_cache(CacheEntry {
            key: CacheKey {
                client: lr_runner::WORKSPACE_CLIENT.into(),
                project: ws.file_name().unwrap().to_string_lossy().into_owned(),
                name: "mirror".into(),
            },
            path: mirror.display().to_string(),
            bytes: 0,
            last_used: lr_core::Timestamp(START),
        })
        .await
        .unwrap();
    let (_c, _h) = rig.start();
    let _b = Broker::accept(&rig).await;
    rig.settle().await;
    assert_eq!(rig.files.file(&left.join("Cargo.toml")), None);
    assert!(!rig.files.is_dir(&left));
    assert!(rig.files.is_dir(&mirror));
}
