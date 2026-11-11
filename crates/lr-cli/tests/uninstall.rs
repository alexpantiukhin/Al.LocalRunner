mod common;

use common::TempDir;
use lr_cli::commands::setup::{purge_confirmed, uninstall_plan};
use lr_cli::os_service::{Action, OsSystem, System};
use lr_cli::paths::Layout;

fn layout(dir: &TempDir) -> Layout {
    let root = dir.path().join("state");
    std::fs::create_dir_all(root.join("runner").join("r1")).unwrap();
    std::fs::write(root.join("runner").join("r1").join("runner.key"), "k").unwrap();
    std::fs::write(root.join("broker.db"), "db").unwrap();
    Layout::new(root.clone(), root)
}

fn ask(layout: &Layout, yes: bool, terminal: bool, typed: &str) -> (Result<bool, String>, String) {
    let mut input = typed.as_bytes();
    let mut prompt = Vec::new();
    let answer = purge_confirmed(layout, yes, terminal, &mut input, &mut prompt);
    (answer, String::from_utf8(prompt).unwrap())
}

#[test]
fn purge_needs_yes_or_a_typed_yes() {
    let dir = TempDir::new("purge-confirm");
    let layout = layout(&dir);
    assert_eq!(ask(&layout, true, false, ""), (Ok(true), String::new()));
    let (refused, _) = ask(&layout, false, false, "yes\n");
    assert_eq!(
        refused,
        Err("--purge deletes the state directory; confirm with --yes".into()),
        "without a terminal nothing is read"
    );
    let (answer, prompt) = ask(&layout, false, true, "no\n");
    assert_eq!(answer, Ok(false));
    assert_eq!(
        prompt,
        format!(
            "delete {} with keys, databases and logs? type yes: ",
            layout.root.display()
        )
    );
    assert_eq!(ask(&layout, false, true, "y\n").0, Ok(false));
    assert_eq!(ask(&layout, false, true, "").0, Ok(false));
    assert_eq!(ask(&layout, false, true, " yes \n").0, Ok(true));
}

#[test]
fn purge_removes_the_state_directory_and_revokes_nothing_at_the_broker() {
    let dir = TempDir::new("purge-plan");
    let layout = layout(&dir);
    let bin = dir.path().join("bin");
    let kept = uninstall_plan(&layout, bin.clone(), false, false).unwrap();
    assert!(
        !kept.iter().any(|a| matches!(a, Action::RemoveDirAll(_))),
        "keys and history stay without --purge: {kept:?}"
    );
    let purged = uninstall_plan(&layout, bin, false, true).unwrap();
    assert_eq!(&purged[..kept.len()], &kept[..], "the same steps first");
    assert_eq!(
        &purged[kept.len()..],
        [Action::RemoveDirAll(layout.root.clone())]
    );
    for a in &purged {
        if let Action::Run { program, args, .. } = a {
            let line = format!("{program} {}", args.join(" "));
            assert!(
                !line.contains("localrunner") && !line.contains("runner remove"),
                "uninstall never reaches the broker: {line}"
            );
        }
    }
    OsSystem
        .apply(purged.last().unwrap())
        .expect("the state directory is removed");
    assert!(!layout.root.exists());
}
