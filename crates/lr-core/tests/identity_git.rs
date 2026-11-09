mod common;

use common::{git, init_repo, session, TempDir};

#[test]
fn no_remote_uses_repo_root_name() {
    let tmp = TempDir::new("plain-repo");
    init_repo(tmp.path());
    let sub = tmp.path().join("nested");
    std::fs::create_dir(&sub).unwrap();
    let id = session("alex", None, &sub).unwrap();
    assert_eq!(id.project, "plain-repo");
    assert!(id.branch.is_some());
    assert_eq!(
        id.worktree_root.as_deref().unwrap().file_name().unwrap(),
        "plain-repo"
    );
    assert_eq!(
        id.repo_root.as_deref().unwrap().file_name().unwrap(),
        "plain-repo"
    );
}

#[test]
fn origin_gives_owner_repo_project() {
    let tmp = TempDir::new("whatever");
    init_repo(tmp.path());
    git(
        tmp.path(),
        &["remote", "add", "origin", "git@github.com:owner/repo.git"],
    );
    assert_eq!(
        session("alex", None, tmp.path()).unwrap().project,
        "owner/repo"
    );
}

#[test]
fn https_origin_without_git_suffix_gives_owner_repo_project() {
    let tmp = TempDir::new("x");
    init_repo(tmp.path());
    git(
        tmp.path(),
        &["remote", "add", "origin", "https://github.com/owner/repo"],
    );
    assert_eq!(
        session("alex", None, tmp.path()).unwrap().project,
        "owner/repo"
    );
}

#[test]
fn detached_head_has_no_branch() {
    let tmp = TempDir::new("detached");
    init_repo(tmp.path());
    git(tmp.path(), &["checkout", "-q", "--detach"]);
    assert_eq!(session("alex", None, tmp.path()).unwrap().branch, None);
}

#[test]
fn linked_worktree_records_both_roots() {
    let tmp = TempDir::new("main-repo");
    init_repo(tmp.path());
    let linked = tmp.path().parent().unwrap().join("linked-wt");
    git(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().unwrap(),
            "-b",
            "side",
        ],
    );
    let id = session("alex", None, &linked).unwrap();
    assert_eq!(id.branch.as_deref(), Some("side"));
    assert_eq!(
        id.worktree_root.as_deref().unwrap().file_name().unwrap(),
        "linked-wt"
    );
    assert_eq!(
        id.repo_root.as_deref().unwrap().file_name().unwrap(),
        "main-repo"
    );
    assert_eq!(id.project, "main-repo");
}

#[test]
fn path_with_spaces_and_cyrillic_gives_project_and_roots() {
    let tmp = TempDir::new("Мой проект (тест)");
    init_repo(tmp.path());
    let id = session("alex", None, tmp.path()).unwrap();
    assert_eq!(id.project, "Мой проект (тест)");
    let root = id.worktree_root.unwrap();
    assert_eq!(root.file_name().unwrap(), "Мой проект (тест)");
    assert!(root.is_dir());
    assert_eq!(
        id.repo_root.unwrap().file_name().unwrap(),
        "Мой проект (тест)"
    );
    let sub = tmp.path().join("под папка");
    std::fs::create_dir(&sub).unwrap();
    assert_eq!(
        session("alex", None, &sub).unwrap().project,
        "Мой проект (тест)"
    );
}

#[test]
fn linked_worktree_with_spaces_and_cyrillic_keeps_main_repo_root() {
    let tmp = TempDir::new("Основной репо");
    init_repo(tmp.path());
    let linked = tmp.path().parent().unwrap().join("доп дерево");
    git(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().unwrap(),
            "-b",
            "side",
        ],
    );
    let id = session("alex", None, &linked).unwrap();
    assert_eq!(id.project, "Основной репо");
    assert_eq!(id.worktree_root.unwrap().file_name().unwrap(), "доп дерево");
    assert_eq!(id.repo_root.unwrap().file_name().unwrap(), "Основной репо");
}
