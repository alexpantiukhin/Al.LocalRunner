#![allow(dead_code)]

use lr_store::{SqliteBrokerStore, SqliteHubStore, SqliteRunnerStore};
use std::path::PathBuf;
use tempfile::TempDir;

pub fn dir() -> TempDir {
    tempfile::tempdir().expect("temporary directory")
}

pub fn broker() -> SqliteBrokerStore {
    let d = dir();
    SqliteBrokerStore::open(&d.path().join("broker.db"))
        .unwrap()
        .keep_alive(d)
}

pub fn hub() -> SqliteHubStore {
    let d = dir();
    SqliteHubStore::open(&d.path().join("hub.db"))
        .unwrap()
        .keep_alive(d)
}

pub fn runner() -> SqliteRunnerStore {
    let d = dir();
    SqliteRunnerStore::open(&d.path().join("runner.db"))
        .unwrap()
        .keep_alive(d)
}

pub fn db_path(d: &TempDir, name: &str) -> PathBuf {
    d.path().join(name)
}
