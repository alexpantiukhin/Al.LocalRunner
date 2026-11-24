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
