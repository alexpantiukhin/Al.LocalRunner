//! Schema versions of the three databases and the opening procedure: WAL mode, foreign keys,
//! a kind marker (`application_id`) and a version (`user_version`) that is checked, migrated
//! step by step, or refused when newer than this build knows. Before a migration the file is
//! copied to `<name>.bak-<old version>` (owner-only on Unix) and only this latest copy is kept;
//! a failed copy stops the migration.

use lr_abstractions::StoreError;
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use std::path::{Path, PathBuf};

/// One step from schema version `n` to `n + 1`, run inside the opening transaction.
pub type Migration = fn(&Transaction<'_>) -> rusqlite::Result<()>;

/// Description of one database file: how a new one is created and how an old one is brought
/// up to date. `migrations[i]` moves version `i + 1` to `i + 2`, so a schema of version `v`
/// has `v - 1` migrations.
#[derive(Debug)]
pub struct Schema {
    /// Name used in messages (`broker.db`).
    pub name: &'static str,
    /// Marker written into the file so a hub database is never opened as a broker's.
    pub application_id: i32,
    /// Current version, written to `PRAGMA user_version`.
    pub version: u32,
    /// DDL of the current version, run on a new file.
    pub create: &'static str,
    /// Steps from older versions.
    pub migrations: &'static [Migration],
}

/// Schema of `broker.db`.
pub static BROKER: Schema = Schema {
    name: "broker.db",
    application_id: 0x4c52_4231,
    version: 1,
    create: include_str!("sql/broker.sql"),
    migrations: &[],
};

/// Schema of `hub.db`.
pub static HUB: Schema = Schema {
    name: "hub.db",
    application_id: 0x4c52_4832,
    version: 2,
    create: include_str!("sql/hub.sql"),
    migrations: &[hub_v2],
};

fn hub_v2(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "ALTER TABLE active_order ADD COLUMN spec_json TEXT;
         CREATE TABLE hub_meta (name TEXT PRIMARY KEY, value INTEGER NOT NULL);",
    )
}

/// Schema of `runner.db`.
pub static RUNNER: Schema = Schema {
    name: "runner.db",
    application_id: 0x4c52_5233,
    version: 1,
    create: include_str!("sql/runner.sql"),
    migrations: &[],
};

fn fail(path: &Path, schema: &Schema, what: impl std::fmt::Display) -> StoreError {
    StoreError::Io(format!("{} ({}): {what}", path.display(), schema.name))
}

/// Opens (and with `create`, makes) the database file at `path` and brings it to the current
/// version of `schema`.
pub fn open(path: &Path, schema: &Schema, create: bool) -> Result<Connection, StoreError> {
    if !create && !path.is_file() {
        return Err(fail(path, schema, "file does not exist"));
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| fail(path, schema, format!("directory: {e}")))?;
    }
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if create {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let mut conn = Connection::open_with_flags(path, flags)
        .map_err(|e| fail(path, schema, format!("cannot open: {e}")))?;
    prepare(&mut conn, path, schema, create).map_err(|e| match e {
        Prepare::Store(msg) => fail(path, schema, msg),
        Prepare::Sql(e) => fail(
            path,
            schema,
            format!("not a usable SQLite database ({e}); the file is damaged or is not a localrunner database"),
        ),
    })?;
    Ok(conn)
}

enum Prepare {
    Store(String),
    Sql(rusqlite::Error),
}

impl From<rusqlite::Error> for Prepare {
    fn from(e: rusqlite::Error) -> Prepare {
        Prepare::Sql(e)
    }
}

fn prepare(
    conn: &mut Connection,
    path: &Path,
    schema: &Schema,
    create: bool,
) -> Result<(), Prepare> {
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let application_id: i32 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let tables: i64 = conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
    let fresh = application_id == 0 && version == 0 && tables == 0;
    if fresh && !create {
        return Err(Prepare::Store("database is empty".into()));
    }
    if !fresh && application_id != schema.application_id {
        return Err(Prepare::Store(format!(
            "this is not a {} (application id {application_id:#x})",
            schema.name
        )));
    }
    if version == 0 && !fresh {
        return Err(Prepare::Store(
            "tables exist but the schema version is not set".into(),
        ));
    }
    if version > schema.version {
        return Err(Prepare::Store(format!(
            "schema version {version} is newer than the {} this build supports; \
             upgrade localrunner instead of opening it with an old build",
            schema.version
        )));
    }
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(Prepare::Store(format!(
            "WAL mode is not available (got {mode})"
        )));
    }
    conn.pragma_update(None, "foreign_keys", "ON")?;
    if fresh {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
            &schema
                .create
                .replace("{FINAL}", &crate::codec::final_list()),
        )?;
        stamp(&tx, schema, schema.version)?;
        tx.commit()?;
    } else if version < schema.version {
        migrate(conn, path, schema, version)?;
    }
    Ok(())
}

fn stamp(tx: &Transaction<'_>, schema: &Schema, version: u32) -> rusqlite::Result<()> {
    tx.pragma_update(None, "application_id", schema.application_id)?;
    tx.pragma_update(None, "user_version", version)
}

fn migrate(conn: &mut Connection, path: &Path, schema: &Schema, from: u32) -> Result<(), Prepare> {
    let steps = (from..schema.version)
        .map(|step| {
            schema
                .migrations
                .get(step as usize - 1)
                .ok_or_else(|| Prepare::Store(format!("no migration from version {step}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    backup(conn, path, from, schema.version)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for migration in steps {
        migration(&tx)?;
    }
    stamp(&tx, schema, schema.version)?;
    tx.commit()?;
    Ok(())
}

/// Path of the copy made before migrating the file at `path` from schema version `from`.
pub fn backup_path(path: &Path, from: u32) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".bak-{from}"));
    path.with_file_name(name)
}

fn backup(conn: &Connection, path: &Path, from: u32, to: u32) -> Result<(), Prepare> {
    let bak = backup_path(path, from);
    let refuse = |what: String| {
        Prepare::Store(format!(
            "backup {} before migrating schema version {from} to {to} failed: {what}; \
             the database is left at version {from}",
            bak.display()
        ))
    };
    let target = bak
        .to_str()
        .ok_or_else(|| refuse("the path is not valid UTF-8".into()))?
        .to_string();
    match std::fs::remove_file(&bak) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(refuse(e.to_string())),
        _ => {}
    }
    create_owner_only(&bak).map_err(|e| refuse(e.to_string()))?;
    if let Err(e) = conn.execute("VACUUM INTO ?1", [&target]) {
        let _ = std::fs::remove_file(&bak);
        return Err(refuse(e.to_string()));
    }
    drop_older_backups(path, &bak);
    Ok(())
}

fn drop_older_backups(path: &Path, keep: &Path) {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let prefix = format!("{}.bak-", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let other = entry.path();
        let ours = entry
            .file_name()
            .to_str()
            .and_then(|n| n.strip_prefix(&prefix))
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()));
        let plain = entry
            .file_type()
            .is_ok_and(|t| t.is_file() || t.is_symlink());
        if ours && plain && other != keep {
            let _ = std::fs::remove_file(&other);
        }
    }
}

#[cfg(unix)]
fn create_owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map(drop)
}

#[cfg(not(unix))]
fn create_owner_only(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(drop)
}
