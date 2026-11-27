mod common;

use common::TempDir;
use lr_abstractions::{SecretError, SecretSource};
use lr_cli::runner_config::SecretFrom;
use lr_cli::secrets::ConfigSecrets;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn secrets(path: PathBuf) -> ConfigSecrets {
    ConfigSecrets::new(BTreeMap::from([(
        "tok".to_string(),
        SecretFrom::File(path),
    )]))
}

#[tokio::test]
async fn a_file_secret_is_the_file_without_its_last_line_break() {
    let dir = TempDir::new("secret-file");
    for (content, want) in [
        ("fake-one", "fake-one"),
        ("fake-two\n", "fake-two"),
        ("fake-three\r\n", "fake-three"),
        ("fake\nfour\n\n", "fake\nfour\n"),
    ] {
        let path = dir.path().join("tok");
        std::fs::write(&path, content).unwrap();
        let value = secrets(path).get("tok").await.unwrap();
        assert_eq!(value.expose(), want);
    }
}

#[tokio::test]
async fn a_missing_or_empty_file_is_not_found_and_a_binary_one_is_unavailable_without_its_bytes() {
    let dir = TempDir::new("secret-file-bad");
    let path = dir.path().join("tok");
    assert_eq!(
        secrets(path.clone()).get("tok").await.unwrap_err(),
        SecretError::NotFound("tok".into())
    );
    std::fs::write(&path, "\n").unwrap();
    assert_eq!(
        secrets(path.clone()).get("tok").await.unwrap_err(),
        SecretError::NotFound("tok".into())
    );
    std::fs::write(&path, [0xff, 0xfe, b'x']).unwrap();
    let e = secrets(path).get("tok").await.unwrap_err();
    assert!(
        matches!(&e, SecretError::Unavailable { reason, .. } if reason.contains("UTF-8")),
        "{e}"
    );
    let other = secrets(dir.path().join("x"))
        .get("other")
        .await
        .unwrap_err();
    assert_eq!(other, SecretError::NotFound("other".into()));
}

#[test]
fn unavailable_lists_secrets_whose_source_gives_nothing_without_values() {
    let dir = TempDir::new("secret-unavailable");
    let present = dir.path().join("present");
    std::fs::write(&present, "value\n").unwrap();
    let secrets = ConfigSecrets::new(std::collections::BTreeMap::from([
        ("a_env".to_string(), SecretFrom::Env("LR_TEST_SURELY_UNSET_VAR_8F3A".into())),
        ("b_file".to_string(), SecretFrom::File(dir.path().join("missing"))),
        ("c_ok".to_string(), SecretFrom::File(present)),
    ]));
    let found = secrets.unavailable();
    let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["a_env", "b_file"]);
    assert!(found[0].1.contains("LR_TEST_SURELY_UNSET_VAR_8F3A is not set"), "{found:?}");
    assert!(found[1].1.contains("does not exist"), "{found:?}");
}
