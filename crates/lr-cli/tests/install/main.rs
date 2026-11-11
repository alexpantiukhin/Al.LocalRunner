#[path = "../common/mod.rs"]
mod common;
mod container;
mod scripts;
mod scripts_ps;
mod scripts_run;
mod yaml;

use std::path::PathBuf;

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn install_file(name: &str) -> String {
    let path = repo_root().join("install").join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()))
        .replace("\r\n", "\n")
}
