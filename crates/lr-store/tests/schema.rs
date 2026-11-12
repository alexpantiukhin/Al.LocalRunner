mod common;

use lr_abstractions::StoreError;
use lr_store::schema::{self, Schema};
use lr_store::{SqliteBrokerStore, SqliteHubStore, SqliteRunnerStore};
use rusqlite::{Connection, Transaction};
use std::collections::BTreeMap;
use std::path::Path;

fn columns(path: &Path) -> BTreeMap<String, Vec<String>> {
    let conn = Connection::open(path).unwrap();
    let mut tables = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .unwrap();
    let names: Vec<String> = tables
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    names
        .into_iter()
        .map(|t| {
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info(\"{t}\")"))
                .unwrap();
            let cols = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            (t, cols)
        })
        .collect()
}

fn expect(tables: &[(&str, &str)]) -> BTreeMap<String, Vec<String>> {
    tables
        .iter()
        .map(|(t, c)| (t.to_string(), c.split(", ").map(str::to_string).collect()))
        .collect()
}

#[test]
fn broker_db_has_the_tables_and_columns_of_the_er_diagram() {
    let d = common::dir();
    let path = d.path().join("broker.db");
    SqliteBrokerStore::open(&path).unwrap();
    let want = expect(&[
        ("job", "client_key, id, project, source, label, note, branch, cwd, profile, platform, runner_name, commit_sha, client_machine, client_label, os_user, secrets, code_source, queued_at, started_at, finished_at, state, exit_code, cancelled_by, cancel_reason, peak_mem_mb, peak_disk_mb, recv_seq_hub, recv_seq_runner, send_seq_runner"),
        ("step", "client_key, job_id, ord, name, command, state, exit_code, started_at, finished_at, timeout_s"),
        ("job_spec", "client_key, job_id, resolved_json, profile_commit, order_json"),
        ("github_run", "client_key, job_id, repository, workflow, job_name, run_id, attempt, ref, actor, run_url"),
        ("job_log", "client_key, job_id, path, bytes, updated_at"),
        ("machine", "name, machine_id, kind, label, public_key, registered_at, last_seen, revoked_at"),
        ("join_code", "code_hash, kind, name, label, uses_left, issued_at, expires_at, redeemed_at, machine_name"),
        ("runner", "name, machine_name, programs, platforms, tags, max_timeout_s, cache_max_mb, cache_used_mb, free_disk_mb, services, accept, last_seen, state, enforcement"),
        ("result_outbox", "client_machine, job_id, seq, kind, payload_json, created_at, acked_at"),
        ("order", "client_key, job_id, client_machine, spec_json, platform, same_machine, priority_hint, enqueued_at, state"),
    ]);
    assert_eq!(columns(&path), want);
}

#[test]
fn hub_db_has_the_tables_and_columns_of_the_er_diagram() {
    let d = common::dir();
    let path = d.path().join("hub.db");
    SqliteHubStore::open(&path).unwrap();
    let want = expect(&[
        (
            "outbox",
            "job_id, seq, kind, payload_json, created_at, acked_at",
        ),
        (
            "active_order",
            "job_id, broker, last_seq, label, os_user, submitter_pid, submitter_start, handle_path, detached, spec_json",
        ),
        ("hub_meta", "name, value"),
        (
            "broker_link",
            "broker, address, fingerprint, key_id, machine_id, last_zone",
        ),
    ]);
    assert_eq!(columns(&path), want);
}

#[test]
fn runner_db_has_the_tables_and_columns_of_the_er_diagram() {
    let d = common::dir();
    let path = d.path().join("runner.db");
    SqliteRunnerStore::open(&path).unwrap();
    let want = expect(&[
        (
            "runner_outbox",
            "job_id, seq, kind, payload_json, created_at, acked_at",
        ),
        ("runner_recv", "job_id, recv_seq"),
        (
            "service_registry",
            "name, pid, pid_start, state, started_at, expires_at, mem_mb",
        ),
        (
            "cache_index",
            "client, project, name, path, bytes, last_used",
        ),
    ]);
    assert_eq!(columns(&path), want);
}

#[test]
fn new_database_is_wal_with_version_and_kind_marker() {
    let d = common::dir();
    let path = d.path().join("broker.db");
    SqliteBrokerStore::open(&path).unwrap();
    let conn = Connection::open(&path).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    let kind: i32 = conn
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    assert_eq!(version, schema::BROKER.version);
    assert_eq!(kind, schema::BROKER.application_id);
}

fn io_message<T>(result: Result<T, StoreError>) -> String {
    match result {
        Err(StoreError::Io(msg)) => msg,
        Err(other) => panic!("unexpected error {other:?}"),
        Ok(_) => panic!("the open must be refused"),
    }
}

#[test]
fn newer_schema_version_is_refused_with_a_clear_error_and_left_untouched() {
    let d = common::dir();
    let path = d.path().join("hub.db");
    SqliteHubStore::open(&path).unwrap();
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 99)
        .unwrap();
    let msg = io_message(SqliteHubStore::open(&path));
    assert!(msg.contains("schema version 99 is newer"), "{msg}");
    assert!(msg.contains("hub.db"), "{msg}");
    assert_eq!(version_of(&path), 99);
}

#[test]
fn a_database_of_another_kind_is_refused() {
    let d = common::dir();
    let path = d.path().join("hub.db");
    SqliteHubStore::open(&path).unwrap();
    let msg = io_message(SqliteBrokerStore::open(&path));
    assert!(msg.contains("not a broker.db"), "{msg}");
}

#[test]
fn a_foreign_sqlite_file_is_refused() {
    let d = common::dir();
    let path = d.path().join("broker.db");
    Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE other (a INTEGER); PRAGMA user_version = 1;")
        .unwrap();
    let msg = io_message(SqliteBrokerStore::open(&path));
    assert!(msg.contains("not a broker.db"), "{msg}");
}

#[test]
fn a_corrupt_file_gives_an_error_naming_the_file_and_is_left_alone() {
    let d = common::dir();
    let path = d.path().join("broker.db");
    std::fs::write(&path, vec![0x5au8; 4096]).unwrap();
    let msg = io_message(SqliteBrokerStore::open(&path));
    assert!(msg.contains("broker.db"), "{msg}");
    assert!(
        msg.contains("damaged or is not a localrunner database"),
        "{msg}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), vec![0x5au8; 4096]);
}

#[test]
fn a_missing_file_is_created_by_open_and_refused_by_open_existing() {
    let d = common::dir();
    let path = d.path().join("state").join("runner.db");
    let msg = io_message(SqliteRunnerStore::open_existing(&path));
    assert!(msg.contains("file does not exist"), "{msg}");
    assert!(!path.exists());
    SqliteRunnerStore::open(&path).unwrap();
    assert!(path.is_file());
    SqliteRunnerStore::open_existing(&path).unwrap();
}

#[test]
fn an_empty_file_is_not_taken_for_an_existing_database() {
    let d = common::dir();
    let path = d.path().join("hub.db");
    std::fs::write(&path, b"").unwrap();
    let msg = io_message(SqliteHubStore::open_existing(&path));
    assert!(msg.contains("database is empty"), "{msg}");
}

#[test]
fn a_directory_in_place_of_the_file_gives_an_error() {
    let d = common::dir();
    let path = d.path().join("broker.db");
    std::fs::create_dir(&path).unwrap();
    assert!(matches!(
        SqliteBrokerStore::open(&path),
        Err(StoreError::Io(_))
    ));
}

static V1: Schema = Schema {
    name: "t.db",
    application_id: 77,
    version: 1,
    create: "CREATE TABLE t (a INTEGER);",
    migrations: &[],
};

fn add_b(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch("ALTER TABLE t ADD COLUMN b TEXT NOT NULL DEFAULT 'x';")
}

fn broken(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch("ALTER TABLE t ADD COLUMN c TEXT; SELECT * FROM missing_table;")
}

static V2: Schema = Schema {
    name: "t.db",
    application_id: 77,
    version: 2,
    create: "CREATE TABLE t (a INTEGER, b TEXT NOT NULL DEFAULT 'x');",
    migrations: &[add_b],
};

static V2_BROKEN: Schema = Schema {
    name: "t.db",
    application_id: 77,
    version: 2,
    create: "CREATE TABLE t (a INTEGER, c TEXT);",
    migrations: &[broken],
};

static V3_NO_STEP: Schema = Schema {
    name: "t.db",
    application_id: 77,
    version: 3,
    create: "CREATE TABLE t (a INTEGER);",
    migrations: &[add_b],
};

fn version_of(path: &Path) -> u32 {
    Connection::open(path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn an_older_database_is_migrated_step_by_step_keeping_its_rows() {
    let d = common::dir();
    let path = d.path().join("t.db");
    schema::open(&path, &V1, true)
        .unwrap()
        .execute("INSERT INTO t (a) VALUES (7)", [])
        .unwrap();
    let conn = schema::open(&path, &V2, false).unwrap();
    let (a, b): (i64, String) = conn
        .query_row("SELECT a, b FROM t", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!((a, b.as_str()), (7, "x"));
    drop(conn);
    assert_eq!(version_of(&path), 2);
}

#[test]
fn a_failing_migration_rolls_back_and_keeps_the_old_version() {
    let d = common::dir();
    let path = d.path().join("t.db");
    schema::open(&path, &V1, true).unwrap();
    assert!(schema::open(&path, &V2_BROKEN, false).is_err());
    assert_eq!(version_of(&path), 1);
    assert_eq!(columns(&path)["t"], vec!["a".to_string()]);
}

#[test]
fn a_missing_migration_step_is_an_error_not_a_silent_skip() {
    let d = common::dir();
    let path = d.path().join("t.db");
    schema::open(&path, &V1, true).unwrap();
    let msg = io_message(schema::open(&path, &V3_NO_STEP, false));
    assert!(msg.contains("no migration from version 2"), "{msg}");
    assert_eq!(version_of(&path), 1);
}

fn row_count(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn a_migration_is_preceded_by_a_backup_named_after_the_old_version() {
    let d = common::dir();
    let path = d.path().join("t.db");
    schema::open(&path, &V1, true)
        .unwrap()
        .execute("INSERT INTO t (a) VALUES (7)", [])
        .unwrap();
    drop(schema::open(&path, &V2, false).unwrap());
    let bak = d.path().join("t.db.bak-1");
    assert!(bak.is_file(), "backup {} is missing", bak.display());
    assert_eq!(version_of(&bak), 1);
    assert_eq!(row_count(&bak), 1);
    assert_eq!(columns(&bak)["t"], vec!["a".to_string()]);
    assert_eq!(version_of(&path), 2);
}

#[test]
fn no_backup_is_made_when_nothing_is_migrated() {
    let d = common::dir();
    let path = d.path().join("t.db");
    drop(schema::open(&path, &V2, true).unwrap());
    drop(schema::open(&path, &V2, false).unwrap());
    let names: Vec<String> = std::fs::read_dir(d.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".bak"))
        .collect();
    assert!(names.is_empty(), "{names:?}");
}

#[test]
fn a_leftover_backup_of_the_same_version_is_replaced() {
    let d = common::dir();
    let path = d.path().join("t.db");
    schema::open(&path, &V1, true)
        .unwrap()
        .execute("INSERT INTO t (a) VALUES (1), (2)", [])
        .unwrap();
    std::fs::write(d.path().join("t.db.bak-1"), b"stale").unwrap();
    drop(schema::open(&path, &V2, false).unwrap());
    assert_eq!(row_count(&d.path().join("t.db.bak-1")), 2);
}

#[test]
fn a_failed_backup_stops_the_migration_with_a_clear_error() {
    let d = common::dir();
    let path = d.path().join("t.db");
    schema::open(&path, &V1, true).unwrap();
    let blocker = d.path().join("t.db.bak-1");
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("x"), b"x").unwrap();
    let msg = io_message(schema::open(&path, &V2, false));
    assert!(
        msg.contains("backup") && msg.contains("t.db.bak-1"),
        "{msg}"
    );
    assert!(!msg.contains("  "), "{msg}");
    assert_eq!(version_of(&path), 1);
    assert_eq!(columns(&path)["t"], vec!["a".to_string()]);
}

#[cfg(unix)]
#[test]
fn the_backup_is_readable_by_the_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let d = common::dir();
    let path = d.path().join("t.db");
    schema::open(&path, &V1, true).unwrap();
    drop(schema::open(&path, &V2, false).unwrap());
    let mode = std::fs::metadata(d.path().join("t.db.bak-1"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

static V3: Schema = Schema {
    name: "t.db",
    application_id: 77,
    version: 3,
    create: "CREATE TABLE t (a INTEGER, b TEXT NOT NULL DEFAULT 'x', c TEXT);",
    migrations: &[add_b, add_c],
};

fn add_c(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch("ALTER TABLE t ADD COLUMN c TEXT;")
}

#[test]
fn only_the_latest_backup_is_kept() {
    let d = common::dir();
    let path = d.path().join("t.db");
    schema::open(&path, &V1, true).unwrap();
    drop(schema::open(&path, &V2, false).unwrap());
    assert!(d.path().join("t.db.bak-1").is_file());
    std::fs::write(d.path().join("t.db.bak-0"), b"older").unwrap();
    std::fs::write(d.path().join("other.db.bak-1"), b"not ours").unwrap();
    drop(schema::open(&path, &V3, false).unwrap());
    assert!(d.path().join("t.db.bak-2").is_file());
    assert_eq!(version_of(&d.path().join("t.db.bak-2")), 2);
    assert!(!d.path().join("t.db.bak-1").exists());
    assert!(!d.path().join("t.db.bak-0").exists());
    assert!(d.path().join("other.db.bak-1").exists());
}

struct CwdFiles(String);

impl Drop for CwdFiles {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm", ".bak-1", ".bak-2"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0));
        }
    }
}

#[test]
fn backup_survives_when_the_database_path_is_a_bare_file_name() {
    let name = format!(
        "schema-bare-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let _cleanup = CwdFiles(name.clone());
    let path = Path::new(&name);
    schema::open(path, &V1, true).unwrap();
    drop(schema::open(path, &V2, false).unwrap());
    let bak = format!("{name}.bak-1");
    assert!(Path::new(&bak).is_file(), "the fresh backup {bak} is kept");
    assert_eq!(version_of(Path::new(&bak)), 1);
}

#[tokio::test]
async fn a_version_one_broker_database_gets_the_peak_and_enforcement_columns() {
    use lr_abstractions::store_contract::fixtures::{intake, T0};
    use lr_abstractions::BrokerStore;
    let d = common::dir();
    let path = d.path().join("broker.db");
    let i = intake("c1", "j1", "check", T0);
    let key = i.job.key.clone();
    {
        let store = SqliteBrokerStore::open(&path).unwrap();
        store.accept(i).await.unwrap();
    }
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "ALTER TABLE job DROP COLUMN peak_mem_mb;
         ALTER TABLE job DROP COLUMN peak_disk_mb;
         ALTER TABLE runner DROP COLUMN enforcement;
         ALTER TABLE runner DROP COLUMN version;
         PRAGMA user_version = 1;",
    )
    .unwrap();
    drop(conn);
    let store = SqliteBrokerStore::open(&path).unwrap();
    assert_eq!(version_of(&path), schema::BROKER.version);
    let record = store.job(&key).await.unwrap().unwrap();
    assert_eq!(
        (record.job.peak_mem_mb, record.job.peak_disk_mb),
        (None, None)
    );
    assert_eq!(record.job.project, "owner/repo");
    assert!(path.with_file_name("broker.db.bak-1").is_file());
}
