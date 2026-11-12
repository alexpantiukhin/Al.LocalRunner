mod common;

use common::TempDir;
use lr_cli::facts::collect_github;
use lr_core::{GithubInfo, IdentityError, Source};
use std::collections::BTreeMap;

fn gh_env() -> BTreeMap<String, String> {
    [
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_SERVER_URL", "https://github.com"),
        ("GITHUB_REPOSITORY", "owner/repo"),
        ("GITHUB_WORKFLOW", "ci.yml"),
        ("GITHUB_JOB", "rust-test"),
        ("GITHUB_RUN_ID", "4821"),
        ("GITHUB_RUN_ATTEMPT", "2"),
        ("GITHUB_REF", "refs/heads/main"),
        ("GITHUB_ACTOR", "octocat"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

#[test]
fn github_env_sets_default_label() {
    let tmp = TempDir::new("d");
    let id = collect_github(&gh_env(), None, None, tmp.path()).unwrap();
    assert_eq!(id.label, "github:ci.yml/rust-test");
    assert_eq!(id.project, "owner/repo");
}

#[test]
fn explicit_label_overrides_github_default() {
    let tmp = TempDir::new("d");
    let id = collect_github(&gh_env(), Some("  my-ci "), None, tmp.path()).unwrap();
    assert_eq!(id.label, "my-ci");
}

#[test]
fn invalid_explicit_label_is_rejected_for_github() {
    let tmp = TempDir::new("d");
    assert_eq!(
        collect_github(&gh_env(), Some("a\nb"), None, tmp.path()).unwrap_err(),
        IdentityError::LabelInvalid
    );
}

#[test]
fn github_env_fills_all_source_fields() {
    let tmp = TempDir::new("d");
    let id = collect_github(&gh_env(), None, None, tmp.path()).unwrap();
    let expected = GithubInfo {
        repository: "owner/repo".into(),
        workflow: "ci.yml".into(),
        job: "rust-test".into(),
        run_id: 4821,
        attempt: Some(2),
        git_ref: "refs/heads/main".into(),
        actor: "octocat".into(),
        run_url: "https://github.com/owner/repo/actions/runs/4821/attempts/2".into(),
    };
    assert_eq!(id.source, Source::Github(expected));
}

#[test]
fn github_server_url_trailing_slash_is_trimmed() {
    let tmp = TempDir::new("d");
    let mut env = gh_env();
    env.insert(
        "GITHUB_SERVER_URL".into(),
        "https://ghe.example.com/".into(),
    );
    let Source::Github(info) = collect_github(&env, None, None, tmp.path()).unwrap().source else {
        panic!("not github");
    };
    assert_eq!(
        info.run_url,
        "https://ghe.example.com/owner/repo/actions/runs/4821/attempts/2"
    );
}

#[test]
fn github_run_url_without_attempt_and_default_server() {
    let tmp = TempDir::new("d");
    let mut env = gh_env();
    env.remove("GITHUB_RUN_ATTEMPT");
    env.remove("GITHUB_SERVER_URL");
    let Source::Github(info) = collect_github(&env, None, None, tmp.path()).unwrap().source else {
        panic!("not github");
    };
    assert_eq!(info.attempt, None);
    assert_eq!(
        info.run_url,
        "https://github.com/owner/repo/actions/runs/4821"
    );
}

#[test]
fn github_requires_actions_flag() {
    let tmp = TempDir::new("d");
    let mut env = gh_env();
    env.insert("GITHUB_ACTIONS".into(), "false".into());
    assert_eq!(
        collect_github(&env, None, None, tmp.path()).unwrap_err(),
        IdentityError::NotGithubActions
    );
    env.remove("GITHUB_ACTIONS");
    assert_eq!(
        collect_github(&env, None, None, tmp.path()).unwrap_err(),
        IdentityError::NotGithubActions
    );
}

#[test]
fn github_requires_core_variables() {
    let tmp = TempDir::new("d");
    for name in [
        "GITHUB_REPOSITORY",
        "GITHUB_WORKFLOW",
        "GITHUB_JOB",
        "GITHUB_RUN_ID",
    ] {
        let mut env = gh_env();
        env.remove(name);
        assert_eq!(
            collect_github(&env, None, None, tmp.path()).unwrap_err(),
            IdentityError::MissingEnv(name.into()),
            "{name}"
        );
    }
    let mut env = gh_env();
    env.insert("GITHUB_RUN_ID".into(), "abc".into());
    assert_eq!(
        collect_github(&env, None, None, tmp.path()).unwrap_err(),
        IdentityError::MissingEnv("GITHUB_RUN_ID".into())
    );
}

#[test]
fn github_default_label_is_sanitized() {
    let tmp = TempDir::new("d");
    let mut env = gh_env();
    env.insert("GITHUB_WORKFLOW".into(), "w".repeat(150));
    let label = collect_github(&env, None, None, tmp.path()).unwrap().label;
    assert_eq!(label.chars().count(), 100);
    env.insert("GITHUB_WORKFLOW".into(), "a\nb\u{1b}c".into());
    let label = collect_github(&env, None, None, tmp.path()).unwrap().label;
    assert_eq!(label, "github:a b c/rust-test");
}

#[test]
fn github_project_is_repository_outside_git_and_origin_inside() {
    let tmp = TempDir::new("plain-dir");
    let id = collect_github(&gh_env(), None, None, tmp.path()).unwrap();
    assert_eq!(id.project, "owner/repo");
    common::init_repo(tmp.path());
    common::git(
        tmp.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/other/thing.git",
        ],
    );
    let id = collect_github(&gh_env(), None, None, tmp.path()).unwrap();
    assert_eq!(id.project, "other/thing");
}
