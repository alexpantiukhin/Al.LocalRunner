mod common;

use common::TempDir;
use lr_cli::commands::setup::install_from;
use lr_cli::commands::RoleArg;
use lr_cli::os_service::{Action, OsSystem, System};
use lr_cli::paths::Layout;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Default)]
struct UserPath(Mutex<Vec<PathBuf>>);

impl System for UserPath {
    fn apply(&self, action: &Action) -> Result<(), String> {
        match action {
            Action::AddToUserPath(dir) => {
                self.0.lock().unwrap().push(dir.clone());
                Ok(())
            }
            Action::RemoveFromUserPath(_) => Err("not expected".into()),
            other => OsSystem.apply(other),
        }
    }
}

#[test]
fn install_copies_this_binary_runs_init_and_puts_the_directory_on_path_once() {
    let dir = TempDir::new("install-copy");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    let bin = dir.path().join("bin");
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_localrunner"));
    let path = UserPath::default();
    assert_eq!(
        install_from(
            &layout,
            Some(bin.clone()),
            RoleArg::Runner,
            false,
            exe.clone(),
            &path
        ),
        Ok(0)
    );
    let copied = bin.join(exe.file_name().unwrap());
    assert_eq!(
        std::fs::read(&copied).unwrap(),
        std::fs::read(&exe).unwrap(),
        "the installed binary is this binary"
    );
    assert_eq!(*path.0.lock().unwrap(), vec![bin.clone()]);
    assert!(layout.root.is_dir(), "init created the state directory");
    assert!(
        layout.config_file("runner.toml").is_file() || layout.root.join("runner").is_dir(),
        "init created the runner role"
    );
    assert!(
        !layout.config_file("policy.toml").exists(),
        "only the role asked for"
    );
}
