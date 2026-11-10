//! The connection of one database file and the way async code reaches it.
//!
//! Threading model: one `rusqlite::Connection` behind a `std::sync::Mutex`; every store call
//! moves its whole work (a transaction at most) into `tokio::task::spawn_blocking`, so the
//! runtime never waits on SQLite or the file system. SQLite allows one writer anyway; the
//! mutex makes calls of one store run one after another in the order they take the lock, which
//! is what the seq counters and the write-once rules need.

use crate::schema::{self, Schema};
use lr_abstractions::StoreError;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Failure inside a blocking closure: a store-level refusal or a SQLite error.
pub(crate) enum Fail {
    Store(StoreError),
    Sql(rusqlite::Error),
}

impl From<StoreError> for Fail {
    fn from(e: StoreError) -> Fail {
        Fail::Store(e)
    }
}

impl From<rusqlite::Error> for Fail {
    fn from(e: rusqlite::Error) -> Fail {
        Fail::Sql(e)
    }
}

impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Fail {
        Fail::Store(StoreError::Io(e.to_string()))
    }
}

impl From<Fail> for StoreError {
    fn from(f: Fail) -> StoreError {
        match f {
            Fail::Store(e) => e,
            Fail::Sql(e) => StoreError::Io(format!("sqlite: {e}")),
        }
    }
}

pub(crate) type R<T> = Result<T, Fail>;

pub(crate) fn not_found(what: impl Into<String>) -> Fail {
    Fail::Store(StoreError::NotFound(what.into()))
}

pub(crate) fn io_error(what: impl Into<String>) -> Fail {
    Fail::Store(StoreError::Io(what.into()))
}

/// Runs `f` in an immediate (write) transaction and commits when it succeeds.
pub(crate) fn write<T>(conn: &mut Connection, f: impl FnOnce(&Transaction<'_>) -> R<T>) -> R<T> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let out = f(&tx)?;
    tx.commit()?;
    Ok(out)
}

/// Runs `f` in a deferred transaction so several queries see one snapshot.
pub(crate) fn read<T>(conn: &Connection, f: impl FnOnce(&Connection) -> R<T>) -> R<T> {
    let tx = conn.unchecked_transaction()?;
    let out = f(&tx)?;
    tx.commit()?;
    Ok(out)
}

pub(crate) struct Db {
    conn: Arc<Mutex<Option<Connection>>>,
    path: PathBuf,
    schema: &'static Schema,
    keep: Option<Arc<dyn Any + Send + Sync>>,
}

impl Db {
    pub(crate) fn open(
        path: &Path,
        schema: &'static Schema,
        create: bool,
    ) -> Result<Db, StoreError> {
        let conn = schema::open(path, schema, create)?;
        Ok(Db {
            conn: Arc::new(Mutex::new(Some(conn))),
            path: path.to_path_buf(),
            schema,
            keep: None,
        })
    }

    pub(crate) fn keep_alive(mut self, guard: impl Any + Send + Sync) -> Db {
        self.keep = Some(Arc::new(guard));
        self
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn dir(&self) -> PathBuf {
        match self.path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        }
    }

    pub(crate) fn reopen(self) -> Result<Db, StoreError> {
        let Db {
            conn,
            path,
            schema,
            keep,
        } = self;
        let old = conn.lock().unwrap_or_else(|p| p.into_inner()).take();
        drop(old);
        let mut db = Db::open(&path, schema, false)?;
        db.keep = keep;
        Ok(db)
    }

    pub(crate) async fn run<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> R<T> + Send + 'static,
    {
        let conn = self.conn.clone();
        let name = self.schema.name;
        let joined = tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().unwrap_or_else(|p| p.into_inner());
            match guard.as_mut() {
                Some(c) => f(c),
                None => Err(io_error(format!("{name} is closed"))),
            }
        })
        .await;
        match joined {
            Ok(result) => result.map_err(StoreError::from),
            Err(e) => Err(StoreError::Io(format!("{name}: storage task failed: {e}"))),
        }
    }
}
