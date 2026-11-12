mod common;

use common::{session, TempDir};
use lr_core::{IdentityError, Source};

#[test]
fn non_git_dir_uses_dir_name() {
    let tmp = TempDir::new("just-a-dir");
    let id = session("alex", None, tmp.path()).unwrap();
    assert_eq!(id.project, "just-a-dir");
    assert_eq!(id.branch, None);
    assert_eq!(id.worktree_root, None);
    assert_eq!(id.repo_root, None);
}

#[test]
fn session_without_label_rejected() {
    let tmp = TempDir::new("d");
    assert_eq!(
        session("  ", None, tmp.path()).unwrap_err(),
        IdentityError::MissingLabel
    );
}

#[test]
fn session_collects_process_facts() {
    let tmp = TempDir::new("d");
    let id = session("a", None, tmp.path()).unwrap();
    assert_ne!(
        id.sender_pid,
        std::process::id(),
        "the caller, not this process"
    );
    assert_ne!(id.sender_pid, 0, "the test runner has a live parent");
    assert!(!id.os_user.is_empty());
    assert!(!id.host.is_empty());
    assert_eq!(id.source, Source::Session);
    assert_eq!(id.cwd, tmp.path());
    let start = id.sender_start.expect("parent process start time");
    assert_eq!(lr_exec::process_start(id.sender_pid), Some(start));
}

#[test]
fn the_hub_inspector_sees_a_live_session_sender_alive() {
    use lr_abstractions::{Liveness, ProcessInspector};
    let tmp = TempDir::new("d");
    let id = session("a", None, tmp.path()).unwrap();
    let start = id.sender_start.expect("parent process start time");
    assert_eq!(
        lr_exec::OsProcessInspector.liveness(id.sender_pid, start),
        Liveness::Alive,
        "the dead-submitter rule compares the same fingerprint the CLI recorded"
    );
}
