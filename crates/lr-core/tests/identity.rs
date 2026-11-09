use lr_core::{
    identity_from_session, may_cancel, normalize_origin, project_name, validate_label,
    validate_note, Identity, IdentityError, SenderFacts, Source,
};
use std::path::{Path, PathBuf};

fn id(label: &str, user: &str) -> Identity {
    Identity {
        project: "p".into(),
        source: Source::Session,
        label: label.into(),
        note: None,
        os_user: user.into(),
        host: "h".into(),
        sender_pid: 1,
        sender_start: None,
        cwd: PathBuf::from("."),
        branch: None,
        worktree_root: None,
        repo_root: None,
    }
}

fn facts(cwd: &str, repo_root: Option<&str>, origin: Option<&str>) -> SenderFacts {
    SenderFacts {
        os_user: "u".into(),
        host: "h".into(),
        sender_pid: 7,
        sender_start: Some(5),
        cwd: PathBuf::from(cwd),
        branch: Some("b".into()),
        worktree_root: repo_root.map(PathBuf::from),
        repo_root: repo_root.map(PathBuf::from),
        origin: origin.map(String::from),
    }
}

fn session(label: &str, note: Option<&str>) -> Result<Identity, IdentityError> {
    identity_from_session(label, note, facts("/w/dir", None, None))
}

#[test]
fn origin_forms_normalize_equally() {
    let forms = [
        "https://github.com/owner/repo",
        "https://github.com/owner/repo.git",
        "ssh://git@github.com/owner/repo.git",
        "ssh://git@github.com/owner/repo",
        "ssh://git@github.com:22/owner/repo.git",
        "git@github.com:owner/repo.git",
        "git@github.com:owner/repo",
        "https://user:token@github.com/owner/repo.git",
        "https://github.com/owner/repo/",
        "https://github.com/owner/repo.git/",
    ];
    for form in forms {
        assert_eq!(
            normalize_origin(form).as_deref(),
            Some("owner/repo"),
            "{form}"
        );
    }
}

#[test]
fn origin_edge_forms() {
    let cases = [
        ("git@host:owner/sub/repo.git", Some("owner/sub/repo")),
        ("https://host/Owner/Repo", Some("Owner/Repo")),
        ("user:token@host:owner/repo", None),
        ("h:a/b", None),
        ("file:///x/y", None),
        ("/home/me/repo", None),
    ];
    for (form, expected) in cases {
        assert_eq!(normalize_origin(form).as_deref(), expected, "{form}");
    }
}

#[test]
fn unrecognisable_origin_is_none() {
    for form in [
        "",
        "   ",
        "repo",
        "https://github.com/",
        "https://github.com/owner",
        "C:\\work\\repo",
        "C:/work/repo",
    ] {
        assert_eq!(normalize_origin(form), None, "{form}");
    }
}

#[test]
fn credentials_never_survive_normalization() {
    let normalized = normalize_origin("https://user:secret-token@host/owner/repo.git").unwrap();
    assert!(!normalized.contains("secret-token") && !normalized.contains('@'));
}

#[test]
fn project_prefers_origin_then_repo_root_then_dir_name() {
    let dir = Path::new("some/dir");
    let root = Path::new("some/repo-root");
    assert_eq!(project_name(Some("git@h:o/r.git"), Some(root), dir), "o/r");
    assert_eq!(project_name(None, Some(root), dir), "repo-root");
    assert_eq!(project_name(Some("garbage"), Some(root), dir), "repo-root");
    assert_eq!(project_name(None, None, dir), "dir");
}

#[test]
fn windows_paths_with_spaces_and_cyrillic_give_project_name() {
    let root = Path::new(r"C:\Мои проекты\Проект Один");
    let dir = Path::new(r"C:\Мои проекты\Проект Один\под папка");
    if cfg!(windows) {
        assert_eq!(project_name(None, Some(root), dir), "Проект Один");
        assert_eq!(project_name(None, None, dir), "под папка");
    }
    let unix_root = Path::new("/home/me/Мои проекты/Проект Один");
    assert_eq!(
        project_name(None, Some(unix_root), Path::new("/x")),
        "Проект Один"
    );
    assert_eq!(project_name(Some("git@h:a/b.git"), Some(root), dir), "a/b");
}

#[test]
fn session_without_label_rejected() {
    for label in ["", "   ", "\t\n"] {
        assert_eq!(
            session(label, None).unwrap_err(),
            IdentityError::MissingLabel
        );
    }
}

#[test]
fn label_validation() {
    assert_eq!(
        validate_label("  claude:сессия-3 ").unwrap(),
        "claude:сессия-3"
    );
    assert!(validate_label(&"я".repeat(100)).is_ok());
    assert_eq!(
        validate_label(&"я".repeat(101)).unwrap_err(),
        IdentityError::LabelTooLong
    );
    assert_eq!(
        validate_label("bad\nlabel").unwrap_err(),
        IdentityError::LabelInvalid
    );
}

#[test]
fn note_over_200_chars_rejected() {
    let ok = "ж".repeat(200);
    assert_eq!(
        validate_note(Some(&ok)).unwrap().as_deref(),
        Some(ok.as_str())
    );
    assert_eq!(
        validate_note(Some(&"ж".repeat(201))).unwrap_err(),
        IdentityError::NoteTooLong
    );
    assert_eq!(validate_note(Some("  ")).unwrap(), None);
    assert_eq!(validate_note(None).unwrap(), None);
}

#[test]
fn session_note_is_kept_on_identity() {
    let id = session("a", Some(" зачем ")).unwrap();
    assert_eq!(id.note.as_deref(), Some("зачем"));
}

#[test]
fn own_label_and_user_cancel_without_force() {
    assert!(may_cancel(&id("alex", "u"), &id("alex", "u"), false));
}

#[test]
fn foreign_label_cancel_needs_force() {
    let me = id("alex", "u");
    let other = id("claude:1", "u");
    assert!(!may_cancel(&me, &other, false));
    assert!(may_cancel(&me, &other, true));
}

#[test]
fn foreign_os_user_cancel_needs_force() {
    let me = id("alex", "u1");
    let other = id("alex", "u2");
    assert!(!may_cancel(&me, &other, false));
    assert!(may_cancel(&me, &other, true));
}

#[test]
fn project_comes_from_origin_then_repo_root_then_dir_in_session_identity() {
    let with_origin = identity_from_session(
        "a",
        None,
        facts("/r/sub", Some("/r"), Some("git@h:o/r.git")),
    );
    assert_eq!(with_origin.unwrap().project, "o/r");
    let no_origin = identity_from_session("a", None, facts("/r/sub", Some("/r"), None));
    assert_eq!(no_origin.unwrap().project, "r");
    let outside = identity_from_session("a", None, facts("/x/just-a-dir", None, None));
    assert_eq!(outside.unwrap().project, "just-a-dir");
}

#[test]
fn session_identity_keeps_collected_facts() {
    let id = identity_from_session("a", None, facts("/w", Some("/w"), None)).unwrap();
    assert_eq!(
        (
            id.os_user.as_str(),
            id.host.as_str(),
            id.sender_pid,
            id.sender_start
        ),
        ("u", "h", 7, Some(5))
    );
    assert_eq!(id.source, Source::Session);
    assert_eq!(id.branch.as_deref(), Some("b"));
}
