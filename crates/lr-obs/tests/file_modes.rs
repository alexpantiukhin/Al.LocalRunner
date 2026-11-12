use lr_obs::fs::{LogFs, OsLogFs};
use std::path::PathBuf;

fn fresh_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("lr-obs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[cfg(unix)]
#[test]
fn new_log_directories_are_private_to_the_owner_on_unix() {
    use std::os::unix::fs::PermissionsExt;
    let root = fresh_root("dirmode");
    let logs = root.join("state").join("logs");
    OsLogFs.create_dir_all(&logs).unwrap();
    for dir in [root.join("state"), logs.clone()] {
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{}", dir.display());
    }
    let (_, _) = OsLogFs.open_append(&logs.join("x.log")).unwrap();
    let mode = std::fs::metadata(logs.join("x.log"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(windows)]
#[test]
fn new_log_directories_and_files_only_inherit_the_acl_on_windows() {
    let root = fresh_root("acl");
    let logs = root.join("logs");
    OsLogFs.create_dir_all(&logs).unwrap();
    let file = logs.join("x.log");
    let (_, _) = OsLogFs.open_append(&file).unwrap();
    for path in [&logs, &file] {
        let out = match std::process::Command::new("icacls").arg(path).output() {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            other => {
                if std::env::var("LOCALRUNNER_REQUIRE_TEST_TOOLS").is_ok() {
                    panic!("icacls unavailable: {other:?}");
                }
                eprintln!("SKIPPED: icacls unavailable ({other:?}), ACL not checked");
                return;
            }
        };
        let entries: Vec<&str> = out
            .lines()
            .map(|l| l.trim())
            .filter(|l| l.contains(":("))
            .collect();
        assert!(!entries.is_empty(), "{out}");
        for entry in entries {
            assert!(
                entry.contains("(I)"),
                "explicit ACE on {}: {entry}",
                path.display()
            );
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}
