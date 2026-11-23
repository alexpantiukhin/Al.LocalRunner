use lr_abstractions::fakes::ManualClock;
use lr_cli::config_edit::{needs_rights, plan, write, Change, Target};
use lr_cli::paths::Layout;
use lr_core::Timestamp;
use std::path::PathBuf;

fn scratch(name: &str) -> (Layout, PathBuf) {
    let dir = std::env::temp_dir().join(format!("lr-cfg-change-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    (Layout::new(dir.clone(), dir.clone()), dir)
}

fn change(path: PathBuf, before: &str, after: &str) -> Change {
    Change {
        path,
        before: before.into(),
        after: after.into(),
        warnings: Vec::new(),
        widens: Vec::new(),
    }
}

#[test]
fn diff_names_the_path_and_the_changed_line() {
    let c = change(
        PathBuf::from("cfg/x.toml"),
        "a = 1\nb = 2\nc = 3\n",
        "a = 1\nb = 5\nc = 3\n",
    );
    let diff = c.diff();
    assert!(
        diff.starts_with("--- cfg/x.toml\n+++ cfg/x.toml\n"),
        "{diff}"
    );
    assert!(diff.contains("-b = 2\n+b = 5\n"), "{diff}");
    assert!(diff.contains(" a = 1\n"), "{diff}");
    assert!(diff.contains("@@ -1,3 +1,3 @@"), "{diff}");
}

#[test]
fn noop_when_value_equal() {
    let c = change(PathBuf::from("x"), "a = 1\n", "a = 1\n");
    assert!(c.is_noop());
    assert!(!change(PathBuf::from("x"), "a = 1\n", "a = 2\n").is_noop());
}

#[test]
fn write_makes_backup_then_replaces() {
    let (_, dir) = scratch("backup");
    let path = dir.join("hub-policy.toml");
    std::fs::write(&path, "old\n").unwrap();
    let clock = ManualClock::new(Timestamp(1_795_459_151_000));
    let backup = write(&change(path.clone(), "old\n", "new\n"), &clock).unwrap();
    assert_eq!(backup, dir.join("hub-policy.toml.bak-20261123T183911"));
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), "old\n");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
    let temps = std::fs::read_dir(&dir)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp-")
        })
        .count();
    assert_eq!(temps, 0);
}

#[test]
fn write_without_existing_file_makes_no_backup() {
    let (_, dir) = scratch("fresh");
    let path = dir.join("logging.toml");
    let clock = ManualClock::new(Timestamp(1_795_459_151_000));
    let back = write(&change(path.clone(), "", "a = 1\n"), &clock).unwrap();
    assert_eq!(back, path);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 1\n");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn write_leaves_original_when_rename_fails() {
    let (_, dir) = scratch("renamefail");
    let path = dir.join("target");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("keep"), "x").unwrap();
    let clock = ManualClock::new(Timestamp(1_795_459_151_000));
    assert!(write(&change(path.clone(), "", "new\n"), &clock).is_err());
    assert!(path.join("keep").exists());
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["target".to_string()]);
}

#[test]
fn widens_lists_allow_elevated() {
    let (layout, dir) = scratch("widens");
    std::fs::write(
        dir.join("runner.toml"),
        "name = \"ci\"\n[env]\npath = \"/bin\"\n",
    )
    .unwrap();
    let c = plan(&layout, &Target::Runner("ci".into()), |d| {
        d.set("allow_elevated", "true", false)
    })
    .unwrap();
    assert_eq!(c.widens, vec!["allow_elevated".to_string()]);
    assert!(c.after.contains("allow_elevated = true"));
    let again = plan(&layout, &Target::Runner("ci".into()), |d| {
        d.set("allow_elevated", "false", false)
    })
    .unwrap();
    assert!(again.widens.is_empty());
}

#[test]
fn plan_reads_a_missing_file_as_empty() {
    let (layout, dir) = scratch("missing");
    let c = plan(&layout, &Target::Hub, |d| {
        d.set("brokers.strategy", "least_loaded", false)
    })
    .unwrap();
    assert_eq!(c.before, "");
    assert_eq!(c.path, dir.join("hub-policy.toml"));
    assert!(c.after.contains("least_loaded"));
}

#[test]
fn plan_refuses_invalid_policy_and_writes_nothing() {
    let (layout, dir) = scratch("invalid");
    assert!(plan(&layout, &Target::Hub, |d| d.set("bogus.key", "1", false)).is_err());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

#[test]
fn needs_rights_is_false_for_a_writable_directory() {
    let (_, dir) = scratch("rights");
    assert!(!needs_rights(&dir.join("sub").join("f.toml")));
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}
