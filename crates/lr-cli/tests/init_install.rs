mod common;

use common::TempDir;
use lr_cli::commands::service_cmd::chosen_roles;
use lr_cli::commands::setup::install_with;
use lr_cli::commands::up_cmd::{up_plan, RunningRoles};
use lr_cli::commands::RoleArg;
use lr_cli::init::{self, NodeRole};
use lr_cli::os_service::{service_installed, Os};
use lr_cli::paths::Layout;

fn layout(dir: &TempDir) -> Layout {
    Layout::new(dir.path().join("state"), dir.path().join("config"))
}

#[test]
fn up_without_a_role_starts_only_the_roles_init_created() {
    let dir = TempDir::new("init-roles");
    let layout = layout(&dir);
    init::init(&layout, &[NodeRole::Broker, NodeRole::Runner], "alex").unwrap();
    let chosen = chosen_roles(&layout, None).unwrap();
    assert_eq!(chosen, vec![NodeRole::Broker, NodeRole::Runner]);
    let plan = up_plan(&layout, &chosen, &RunningRoles::default());
    let heads: Vec<String> = plan.iter().map(|argv| argv[..2].join(" ")).collect();
    assert_eq!(heads, vec!["broker run", "runner run"]);
}

#[test]
fn init_writes_only_into_the_installation_and_enables_no_autostart() {
    let dir = TempDir::new("init-no-autostart");
    let layout = layout(&dir);
    let service = service_installed(Os::current());
    let created = init::init(&layout, &NodeRole::ALL, "alex").unwrap();
    assert!(!created.is_empty());
    for c in &created {
        assert!(
            c.path.starts_with(&layout.root) || c.path.starts_with(&layout.config),
            "{} is outside the installation",
            c.path.display()
        );
    }
    let mut files = Vec::new();
    let mut stack = vec![dir.path().to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                files.push(p);
            }
        }
    }
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            !(name.ends_with(".service") || name.ends_with(".plist") || name.ends_with(".xml")),
            "an autostart entry was written: {}",
            f.display()
        );
    }
    assert_eq!(service_installed(Os::current()), service);
}

#[test]
fn install_as_root_or_administrator_needs_allow_elevated() {
    let dir = TempDir::new("install-elevated");
    let layout = layout(&dir);
    let bin = dir.path().join("bin");
    assert_eq!(
        install_with(&layout, Some(bin.clone()), RoleArg::All, false, true, true),
        Err("install refuses to run as root or administrator; add --allow-elevated".into())
    );
    assert!(!bin.exists());
    assert_eq!(
        install_with(&layout, Some(bin.clone()), RoleArg::All, true, true, true),
        Ok(0)
    );
    assert_eq!(
        install_with(&layout, Some(bin.clone()), RoleArg::All, false, true, false),
        Ok(0)
    );
    assert!(!bin.exists(), "a dry run copies nothing");
}
