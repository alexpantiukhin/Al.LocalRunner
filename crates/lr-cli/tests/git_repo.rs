use lr_cli::git_repo::without_credentials;

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
