mod common;

use common::{git, init_repo, TempDir};
use lr_abstractions::Repository;
use lr_cli::git_repo::{without_credentials, GitRepository};

#[tokio::test]
async fn reads_head_committed_file_and_origin() {
    let dir = TempDir::new("repo");
    init_repo(dir.path());
    std::fs::create_dir_all(dir.path().join(".localrunner")).unwrap();
    std::fs::write(dir.path().join(".localrunner/profiles.toml"), "x = 1\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "profiles"]);
    git(
        dir.path(),
        &["remote", "add", "origin", "https://example.com/o/r.git"],
    );
    let repo = GitRepository;
    let head = repo.head(dir.path()).await.unwrap().expect("a commit");
    assert_eq!(head.len(), 40);
    let shown = repo
        .show(dir.path(), &head, ".localrunner/profiles.toml")
        .await
        .unwrap();
    assert_eq!(shown.as_deref(), Some(&b"x = 1\n"[..]));
    assert_eq!(
        repo.show(dir.path(), &head, "missing.toml").await.unwrap(),
        None
    );
    assert_eq!(
        repo.origin_url(dir.path()).await.unwrap().as_deref(),
        Some("https://example.com/o/r.git")
    );
    assert!(!repo.is_pushed(dir.path(), &head).await.unwrap());
}

#[tokio::test]
async fn refuses_values_that_are_not_a_commit_or_a_plain_path() {
    let dir = TempDir::new("repo");
    init_repo(dir.path());
    let repo = GitRepository;
    for commit in ["--output=x", "HEAD", "abc", "g123456"] {
        assert!(
            repo.show(dir.path(), commit, "a.toml").await.is_err(),
            "{commit}"
        );
        assert!(
            repo.is_pushed(dir.path(), commit).await.is_err(),
            "{commit}"
        );
    }
    let head = repo.head(dir.path()).await.unwrap().unwrap();
    for path in ["../x", "/etc/passwd", "-x", "a\\b", "a b", "a:b"] {
        assert!(repo.show(dir.path(), &head, path).await.is_err(), "{path}");
    }
    assert!(repo
        .head(std::path::Path::new("relative/dir"))
        .await
        .is_err());
}

#[test]
fn origin_credentials_never_leave_the_machine() {
    assert_eq!(
        without_credentials("https://user:TOKEN@github.com/o/r.git"),
        "https://github.com/o/r.git"
    );
    assert_eq!(
        without_credentials("https://TOKEN@github.com/o/r.git"),
        "https://github.com/o/r.git"
    );
    assert_eq!(
        without_credentials("ssh://git:secret@host:22/o/r.git"),
        "ssh://git@host:22/o/r.git"
    );
    assert_eq!(
        without_credentials("git@github.com:o/r.git"),
        "git@github.com:o/r.git"
    );
    assert_eq!(
        without_credentials("https://example.com/a@b/r.git"),
        "https://example.com/a@b/r.git"
    );
}

#[tokio::test]
async fn origin_url_drops_the_token() {
    let dir = TempDir::new("repo");
    init_repo(dir.path());
    git(
        dir.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://bot:ghp_secret@example.com/o/r.git",
        ],
    );
    let url = GitRepository.origin_url(dir.path()).await.unwrap();
    assert_eq!(url.as_deref(), Some("https://example.com/o/r.git"));
}

#[tokio::test]
async fn uncommitted_changes_are_seen_tracked_or_new() {
    let dir = TempDir::new("repo");
    init_repo(dir.path());
    std::fs::write(dir.path().join("a.txt"), "1\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "a"]);
    let repo = GitRepository;
    assert!(!repo.has_uncommitted(dir.path()).await.unwrap());
    std::fs::write(dir.path().join("a.txt"), "2\n").unwrap();
    assert!(repo.has_uncommitted(dir.path()).await.unwrap());
    git(dir.path(), &["checkout", "--", "a.txt"]);
    std::fs::write(dir.path().join("new.txt"), "n\n").unwrap();
    assert!(repo.has_uncommitted(dir.path()).await.unwrap());
}
