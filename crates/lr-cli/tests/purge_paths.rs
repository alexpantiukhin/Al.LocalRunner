mod common;

use common::TempDir;
use lr_cli::commands::setup::apply_uninstall;
use lr_cli::host::MACHINE_ID_FILE;
use lr_cli::os_service::{Action, OsSystem};
use lr_cli::purge::{purge_target, Protected};
use std::path::{Path, PathBuf};

fn state(dir: &TempDir) -> PathBuf {
    let root = dir.path().join("state directory with a long name");
    std::fs::create_dir_all(root.join("logs")).unwrap();
    std::fs::write(root.join(MACHINE_ID_FILE), "id").unwrap();
    root
}

fn home(dir: &TempDir, home: PathBuf) -> Protected {
    let away = dir.path().join("away");
    Protected {
        homes: vec![away.join("home"), home],
        bins: vec![away.join("bin")],
        cwd: Some(away.join("cwd")),
    }
}

fn elsewhere(dir: &TempDir) -> Protected {
    home(dir, dir.path().join("away").join("other"))
}

fn refused_as_home(target: &Path, protected: &Protected) {
    let spelled = protected.homes.last().unwrap();
    assert_eq!(
        purge_target(target, protected),
        Err(format!(
            "--purge refuses to delete {}: it is or holds the home directory {}",
            target.display(),
            spelled.display()
        ))
    );
}

fn link_dir(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("a symbolic link");
    #[cfg(windows)]
    if std::os::windows::fs::symlink_dir(target, link).is_err() {
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(out.status.success(), "mklink /J: {out:?}");
    }
    assert!(link.join(MACHINE_ID_FILE).exists() || link.is_dir());
}

#[test]
fn dots_and_a_trailing_separator_name_the_same_directory() {
    let dir = TempDir::new("purge-dots");
    let root = state(&dir);
    let trailing = PathBuf::from(format!("{}{}", root.display(), std::path::MAIN_SEPARATOR));
    assert_eq!(purge_target(&trailing, &elsewhere(&dir)), Ok(root.clone()));
    let dotted = root.join("logs").join("..").join(".");
    assert_eq!(purge_target(&dotted, &elsewhere(&dir)), Ok(root.clone()));
    refused_as_home(&root, &home(&dir, root.join("logs").join("..")));
    refused_as_home(&root, &home(&dir, trailing));
}

#[test]
fn a_home_reached_through_a_linked_parent_is_refused_both_ways() {
    let dir = TempDir::new("purge-parent-link");
    let root = state(&dir);
    let alias = dir.path().join("alias");
    link_dir(dir.path(), &alias);
    let through = alias.join(root.file_name().unwrap());
    assert!(through.join(MACHINE_ID_FILE).exists());
    refused_as_home(&root, &home(&dir, through.clone()));
    refused_as_home(&through, &home(&dir, root.clone()));
}

#[cfg(windows)]
#[test]
fn windows_spellings_of_the_home_directory_are_refused() {
    let dir = TempDir::new("purge-spelling");
    let root = state(&dir);
    let upper = PathBuf::from(root.display().to_string().to_uppercase());
    refused_as_home(&root, &home(&dir, upper));
    let verbatim = PathBuf::from(format!(r"\\?\{}", root.display()));
    refused_as_home(&root, &home(&dir, verbatim.clone()));
    refused_as_home(&verbatim, &home(&dir, root.clone()));
    match short_name(&root) {
        Some(short) => {
            refused_as_home(&root, &home(&dir, short.clone()));
            refused_as_home(&short, &home(&dir, root.clone()));
        }
        None => {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr(),
                "SKIPPED windows_spellings_of_the_home_directory_are_refused (8.3 part): \
                 this volume makes no 8.3 names"
            );
        }
    }
}

#[cfg(windows)]
fn short_name(path: &Path) -> Option<PathBuf> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("cmd")
        .raw_arg(format!(
            r#"/C for %I in ("{}") do @echo %~sI"#,
            path.display()
        ))
        .output()
        .ok()?;
    let short = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!short.is_empty() && short != path.display().to_string() && Path::new(&short).is_dir())
        .then(|| PathBuf::from(short))
}

#[test]
fn the_state_directory_is_checked_again_right_before_it_is_deleted() {
    let dir = TempDir::new("purge-again");
    let root = state(&dir);
    let protected = elsewhere(&dir);
    let target = purge_target(&root, &protected).expect("a state directory");
    let plan = [Action::RemoveDirAll(target.clone())];
    std::fs::remove_file(root.join(MACHINE_ID_FILE)).unwrap();
    assert_eq!(apply_uninstall(&plan, &OsSystem, &protected), (1, false));
    assert!(
        root.join("logs").is_dir(),
        "a directory changed since the check stays"
    );

    std::fs::write(root.join(MACHINE_ID_FILE), "id").unwrap();
    let moved = home(&dir, root.clone());
    assert_eq!(apply_uninstall(&plan, &OsSystem, &moved), (1, false));
    assert!(root.is_dir());

    assert_eq!(apply_uninstall(&plan, &OsSystem, &protected), (0, true));
    assert!(!root.exists());
}
