use lr_core::{identity_from_github, GithubInfo, IdentityError, SenderFacts, Source};
use std::collections::BTreeMap;

fn facts() -> SenderFacts {
    SenderFacts {
        os_user: "u".into(),
        host: "h".into(),
        sender_pid: 1,
        sender_start: None,
        cwd: std::path::PathBuf::from("/w/plain-dir"),
        branch: None,
        worktree_root: None,
        repo_root: None,
        origin: None,
    }
}

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
    let id = identity_from_github(&gh_env(), None, None, facts()).unwrap();
    assert_eq!(id.label, "github:ci.yml/rust-test");
    assert_eq!(id.project, "owner/repo");
}

#[test]
fn explicit_label_overrides_github_default() {
    let id = identity_from_github(&gh_env(), Some("  my-ci "), None, facts()).unwrap();
    assert_eq!(id.label, "my-ci");
}

#[test]
fn invalid_explicit_label_is_rejected_for_github() {
    assert_eq!(
        identity_from_github(&gh_env(), Some("a\nb"), None, facts()).unwrap_err(),
        IdentityError::LabelInvalid
    );
}

#[test]
fn github_env_fills_all_source_fields() {
    let id = identity_from_github(&gh_env(), None, None, facts()).unwrap();
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
    let mut env = gh_env();
    env.insert(
        "GITHUB_SERVER_URL".into(),
        "https://ghe.example.com/".into(),
    );
    let Source::Github(info) = identity_from_github(&env, None, None, facts())
        .unwrap()
        .source
    else {
        panic!("not github");
    };
    assert_eq!(
        info.run_url,
        "https://ghe.example.com/owner/repo/actions/runs/4821/attempts/2"
    );
}

#[test]
fn github_run_url_without_attempt_and_default_server() {
    let mut env = gh_env();
    env.remove("GITHUB_RUN_ATTEMPT");
    env.remove("GITHUB_SERVER_URL");
    let Source::Github(info) = identity_from_github(&env, None, None, facts())
        .unwrap()
        .source
    else {
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
    let mut env = gh_env();
    env.insert("GITHUB_ACTIONS".into(), "false".into());
    assert_eq!(
        identity_from_github(&env, None, None, facts()).unwrap_err(),
        IdentityError::NotGithubActions
    );
    env.remove("GITHUB_ACTIONS");
    assert_eq!(
        identity_from_github(&env, None, None, facts()).unwrap_err(),
        IdentityError::NotGithubActions
    );
}

#[test]
fn github_requires_core_variables() {
    for name in [
        "GITHUB_REPOSITORY",
        "GITHUB_WORKFLOW",
        "GITHUB_JOB",
        "GITHUB_RUN_ID",
    ] {
        let mut env = gh_env();
        env.remove(name);
        assert_eq!(
            identity_from_github(&env, None, None, facts()).unwrap_err(),
            IdentityError::MissingEnv(name.into()),
            "{name}"
        );
    }
    let mut env = gh_env();
    env.insert("GITHUB_RUN_ID".into(), "abc".into());
    assert_eq!(
        identity_from_github(&env, None, None, facts()).unwrap_err(),
        IdentityError::MissingEnv("GITHUB_RUN_ID".into())
    );
}

#[test]
fn github_default_label_is_sanitized() {
    let mut env = gh_env();
    env.insert("GITHUB_WORKFLOW".into(), "w".repeat(150));
    let label = identity_from_github(&env, None, None, facts())
        .unwrap()
        .label;
    assert_eq!(label.chars().count(), 100);
    env.insert("GITHUB_WORKFLOW".into(), "a\nb\u{1b}c".into());
    let label = identity_from_github(&env, None, None, facts())
        .unwrap()
        .label;
    assert_eq!(label, "github:a b c/rust-test");
}

#[test]
fn github_project_is_repository_without_origin_and_origin_wins() {
    let id = identity_from_github(&gh_env(), None, None, facts()).unwrap();
    assert_eq!(id.project, "owner/repo");
    let mut f = facts();
    f.origin = Some("https://github.com/other/thing.git".into());
    let id = identity_from_github(&gh_env(), None, None, f).unwrap();
    assert_eq!(id.project, "other/thing");
}

#[test]
fn github_note_is_kept_trimmed_and_validated() {
    let id = identity_from_github(&gh_env(), None, Some(" why "), facts()).unwrap();
    assert_eq!(id.note.as_deref(), Some("why"));
    assert_eq!(
        identity_from_github(&gh_env(), None, Some(&"x".repeat(201)), facts()).unwrap_err(),
        IdentityError::NoteTooLong
    );
    assert_eq!(
        identity_from_github(&gh_env(), None, Some("  "), facts())
            .unwrap()
            .note,
        None
    );
}
