#[path = "../common/mod.rs"]
mod common;
mod container;
mod scripts;
mod scripts_ps;
mod scripts_ps_work;
mod scripts_run;
mod yaml;

use std::path::PathBuf;

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn skipped(test: &str, tool: &str, why: &str) {
    let required = std::env::var("LOCALRUNNER_REQUIRE_TEST_TOOLS").unwrap_or_default();
    assert!(
        !required
            .split(',')
            .any(|t| t.trim() == tool || t.trim() == "all"),
        "{test}: LOCALRUNNER_REQUIRE_TEST_TOOLS={required} requires {tool}, but {why}"
    );
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "SKIPPED {test}: {why}");
}

pub fn install_file(name: &str) -> String {
    let path = repo_root().join("install").join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()))
        .replace("\r\n", "\n")
}
