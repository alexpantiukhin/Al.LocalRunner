mod common;

use common::dir;
use lr_store::SqliteBrokerStore;

#[test]
fn new_databases_open_and_an_existing_one_is_reused() {
    let d = dir();
    let path = d.path().join("broker.db");
    drop(SqliteBrokerStore::open(&path).unwrap());
    assert!(path.is_file());
    drop(SqliteBrokerStore::open(&path).unwrap());
    drop(SqliteBrokerStore::open_existing(&path).unwrap());
}

#[cfg(unix)]
#[test]
fn every_database_is_created_readable_by_its_owner_only() {
    use lr_store::{SqliteHubStore, SqliteRunnerStore};
    use std::os::unix::fs::PermissionsExt;
    let d = dir();
    let broker = d.path().join("broker.db");
    let hub = d.path().join("hub.db");
    let runner = d.path().join("runner.db");
    let _b = SqliteBrokerStore::open(&broker).unwrap();
    let _h = SqliteHubStore::open(&hub).unwrap();
    let _r = SqliteRunnerStore::open(&runner).unwrap();
    for path in [broker, hub, runner] {
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{}", path.display());
    }
}
