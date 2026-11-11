#![allow(dead_code)]
pub mod fake_roles;
pub mod loopback;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";
#[cfg(not(windows))]
const NULL_DEVICE: &str = "/dev/null";

/// `[env] path` of a test `runner.toml`: jobs get the `PATH` of the test process, since
/// `[env] path` is the only source of `PATH` of a job.
pub fn env_path_toml() -> String {
    let path = std::env::var("PATH").expect("the test process has PATH");
    format!("[env]\npath = {}\n", toml::Value::String(path))
}

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(name: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let unique = format!(
            "lr-identity-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique).join(name);
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

pub fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .env("GIT_CONFIG_GLOBAL", NULL_DEVICE)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .arg("-c")
        .arg(format!("core.hooksPath={NULL_DEVICE}"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn init_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
}

pub fn session(
    label: &str,
    note: Option<&str>,
    cwd: &Path,
) -> Result<lr_core::Identity, lr_core::IdentityError> {
    lr_cli::facts::collect_session(label, note, cwd)
}
