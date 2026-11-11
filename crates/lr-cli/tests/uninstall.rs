mod common;

use common::TempDir;
use lr_cli::commands::setup::{purge_confirmed, uninstall_plan};
use lr_cli::host::MACHINE_ID_FILE;
use lr_cli::os_service::{Action, OsSystem, System};
use lr_cli::paths::Layout;
use lr_cli::purge::{purge_target, Protected};
use std::path::{Path, PathBuf};

fn layout(dir: &TempDir) -> Layout {
    let root = dir.path().join("state");
    std::fs::create_dir_all(root.join("runner").join("r1")).unwrap();
    std::fs::write(root.join("runner").join("r1").join("runner.key"), "k").unwrap();
    std::fs::write(root.join("broker.db"), "db").unwrap();
    std::fs::write(root.join(MACHINE_ID_FILE), "id").unwrap();
    Layout::new(root.clone(), root)
}

fn ask(root: &Path, yes: bool, terminal: bool, typed: &str) -> (Result<bool, String>, String) {
    let mut input = typed.as_bytes();
    let mut prompt = Vec::new();
    let answer = purge_confirmed(root, yes, terminal, &mut input, &mut prompt);
    (answer, String::from_utf8(prompt).unwrap())
}

fn elsewhere(dir: &TempDir) -> Protected {
    let away = dir.path().join("away");
    Protected {
        homes: vec![away.join("home")],
        bins: vec![away.join("bin")],
        cwd: Some(away.join("cwd")),
    }
}

fn refused(path: &Path, why: &str) -> Result<PathBuf, String> {
    Err(format!(
        "--purge refuses to delete {}: {why}",
        path.display()
    ))
}

#[test]
fn purge_needs_yes_or_a_typed_yes() {
    let dir = TempDir::new("purge-confirm");
    let root = layout(&dir).root;
    assert_eq!(ask(&root, true, false, ""), (Ok(true), String::new()));
    let (answer, _) = ask(&root, false, false, "yes\n");
    assert_eq!(
        answer,
        Err("--purge deletes the state directory; confirm with --yes".into()),
        "without a terminal nothing is read"
    );
    let (answer, prompt) = ask(&root, false, true, "no\n");
    assert_eq!(answer, Ok(false));
    assert_eq!(
        prompt,
        format!(
            "delete {} with keys, databases and logs? type yes: ",
            root.display()
        )
    );
    assert_eq!(ask(&root, false, true, "y\n").0, Ok(false));
    assert_eq!(ask(&root, false, true, "").0, Ok(false));
    assert_eq!(ask(&root, false, true, " yes \n").0, Ok(true));
}

#[test]
fn purge_removes_the_state_directory_and_revokes_nothing_at_the_broker() {
    let dir = TempDir::new("purge-plan");
    let layout = layout(&dir);
    let bin = dir.path().join("bin");
    let target = purge_target(&layout.root, &elsewhere(&dir)).expect("a state directory");
    assert_eq!(target, layout.root);
    let kept = uninstall_plan(&layout, bin.clone(), false, None).unwrap();
    assert!(
        !kept.iter().any(|a| matches!(a, Action::RemoveDirAll(_))),
        "keys and history stay without --purge: {kept:?}"
    );
    let purged = uninstall_plan(&layout, bin, false, Some(target.clone())).unwrap();
    assert_eq!(&purged[..kept.len()], &kept[..], "the same steps first");
    assert_eq!(&purged[kept.len()..], [Action::RemoveDirAll(target)]);
    let os_tools = ["schtasks", "sc.exe", "net", "systemctl", "launchctl"];
    for a in &purged {
        if let Action::Run { program, args, .. } = a {
            assert!(os_tools.contains(&program.as_str()), "{program} {args:?}");
            assert!(
                !args
                    .iter()
                    .any(|x| ["remove", "revoke", "broker", "runner"].contains(&x.as_str())),
                "uninstall never reaches the broker: {program} {args:?}"
            );
        }
    }
    OsSystem
        .apply(purged.last().unwrap())
        .expect("the state directory is removed");
    assert!(!layout.root.exists());
}

#[test]
fn purge_refuses_roots_home_binaries_the_current_directory_and_foreign_directories() {
    let dir = TempDir::new("purge-guard");
    let root = layout(&dir).root;
    let fs_root = PathBuf::from(if cfg!(windows) { r"C:\" } else { "/" });
    assert_eq!(
        purge_target(&fs_root, &elsewhere(&dir)),
        refused(&fs_root, "it is the root of a file system")
    );
    let home = Protected {
        homes: vec![root.clone()],
        ..elsewhere(&dir)
    };
    assert_eq!(
        purge_target(&root, &home),
        refused(
            &root,
            &format!("it is or holds the home directory {}", root.display())
        )
    );
    let user = root.join("alex");
    let above_home = Protected {
        homes: vec![user.clone()],
        ..elsewhere(&dir)
    };
    assert_eq!(
        purge_target(&root, &above_home),
        refused(
            &root,
            &format!("it is or holds the home directory {}", user.display())
        )
    );
    let bin = root.join("bin");
    let bins = Protected {
        bins: vec![dir.path().join("other"), bin.clone()],
        ..elsewhere(&dir)
    };
    assert_eq!(
        purge_target(&root, &bins),
        refused(
            &root,
            &format!("it is or holds the binary directory {}", bin.display())
        )
    );
    let cwd = root.join("runner").join("r1");
    let inside = Protected {
        cwd: Some(cwd.clone()),
        ..elsewhere(&dir)
    };
    assert_eq!(
        purge_target(&root, &inside),
        refused(
            &root,
            &format!("it is or holds the current directory {}", cwd.display())
        )
    );
    let dotted = root.join("runner").join("..").join("..").join("state");
    assert_eq!(
        purge_target(&dotted, &inside),
        refused(
            &root,
            &format!("it is or holds the current directory {}", cwd.display())
        ),
        "`..` cannot walk around the guard"
    );
    let foreign = dir.path().join("documents");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(foreign.join("broker.db"), "not ours").unwrap();
    assert_eq!(
        purge_target(&foreign, &elsewhere(&dir)),
        refused(
            &foreign,
            &format!("it is not a state directory (no {MACHINE_ID_FILE})")
        )
    );
    assert!(foreign.join("broker.db").exists());
    assert!(root.join(MACHINE_ID_FILE).exists());
}

#[test]
fn purge_never_follows_a_symbolic_link_to_the_state_directory() {
    let dir = TempDir::new("purge-link");
    let root = layout(&dir).root;
    let link = dir.path().join("link");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&root, &link);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(&root, &link).or_else(|_| {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&root)
            .output()
            .map(|_| ())
    });
    made.expect("a link to the state directory");
    assert!(link.join(MACHINE_ID_FILE).exists());
    assert_eq!(
        purge_target(&link, &elsewhere(&dir)),
        refused(&link, "it is a symbolic link")
    );
    assert!(root.join(MACHINE_ID_FILE).exists());
}
