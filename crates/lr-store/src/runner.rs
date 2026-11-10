use crate::codec::{col_enum, col_opt_u64, col_time, col_u64, int, opt_int, time, Text};
use crate::db::{not_found, write, Db, R};
use crate::outbox::{self, RUNNER};
use crate::schema;
use crate::seq::decide;
use async_trait::async_trait;
use lr_abstractions::{
    CacheEntry, CacheKey, OutboxEntry, OutboxKind, RunnerStore, ServiceRow, StoreError,
};
use lr_core::{JobId, Timestamp};
use rusqlite::{params, Connection, OptionalExtension};
use std::any::Any;
use std::path::Path;

/// `runner.db` on SQLite: the outbox of output and results, the service registry and the
/// cache index. The per-job counter of what the broker sent lives in `runner_recv`.
pub struct SqliteRunnerStore {
    db: Db,
}

impl SqliteRunnerStore {
    /// Opens `runner.db` at `path`, creating the file and the schema when missing.
    pub fn open(path: &Path) -> Result<SqliteRunnerStore, StoreError> {
        Ok(SqliteRunnerStore {
            db: Db::open(path, &schema::RUNNER, true)?,
        })
    }

    /// Like [`SqliteRunnerStore::open`] but fails when the file does not exist.
    pub fn open_existing(path: &Path) -> Result<SqliteRunnerStore, StoreError> {
        Ok(SqliteRunnerStore {
            db: Db::open(path, &schema::RUNNER, false)?,
        })
    }

    /// Closes the connection and opens the same file again, as after a restart.
    pub fn reopen(self) -> Result<SqliteRunnerStore, StoreError> {
        Ok(SqliteRunnerStore {
            db: self.db.reopen()?,
        })
    }

    /// Ties `guard` (a temporary directory, say) to the life of this handle.
    pub fn keep_alive(self, guard: impl Any + Send + Sync) -> SqliteRunnerStore {
        SqliteRunnerStore {
            db: self.db.keep_alive(guard),
        }
    }

    /// The database file.
    pub fn path(&self) -> &Path {
        self.db.path()
    }
}

fn received(conn: &Connection, job: &JobId) -> R<u64> {
    let n: Option<i64> = conn
        .query_row(
            "SELECT recv_seq FROM runner_recv WHERE job_id = ?1",
            [&job.0],
            |r| r.get(0),
        )
        .optional()?;
    Ok(n.unwrap_or(0) as u64)
}

#[async_trait]
impl RunnerStore for SqliteRunnerStore {
    async fn push_outbox(
        &self,
        job: &JobId,
        kind: OutboxKind,
        payload: String,
        at: Timestamp,
    ) -> Result<u64, StoreError> {
        let job = job.clone();
        self.db
            .run(move |c| {
                write(c, |tx| {
                    outbox::push(tx, RUNNER, "", &job, kind, &payload, at)
                })
            })
            .await
    }

    async fn pending_outbox(&self) -> Result<Vec<OutboxEntry>, StoreError> {
        self.db.run(|c| outbox::pending(c, RUNNER, "")).await
    }

    async fn ack_outbox(&self, job: &JobId, seq: u64) -> Result<usize, StoreError> {
        let job = job.clone();
        self.db
            .run(move |c| write(c, |tx| outbox::close(tx, RUNNER, "", Some(&job), Some(seq))))
            .await
    }

    async fn discard_outbox(&self, job: &JobId) -> Result<usize, StoreError> {
        let job = job.clone();
        self.db
            .run(move |c| write(c, |tx| outbox::close(tx, RUNNER, "", Some(&job), None)))
            .await
    }

    async fn clear_outbox(&self) -> Result<usize, StoreError> {
        self.db
            .run(|c| write(c, |tx| outbox::close(tx, RUNNER, "", None, None)))
            .await
    }

    async fn accept_seq(&self, job: &JobId, seq: u64) -> Result<bool, StoreError> {
        let job = job.clone();
        self.db
            .run(move |c| {
                write(c, |tx| {
                    let next = decide(received(tx, &job)?, seq)?;
                    if next {
                        tx.execute(
                            "INSERT INTO runner_recv (job_id, recv_seq) VALUES (?1, ?2)                              ON CONFLICT (job_id) DO UPDATE SET recv_seq = excluded.recv_seq",
                            params![job.0, int(seq)],
                        )?;
                    }
                    Ok(next)
                })
            })
            .await
    }

    async fn last_seq(&self, job: &JobId) -> Result<u64, StoreError> {
        let job = job.clone();
        self.db.run(move |c| received(c, &job)).await
    }

    async fn forget_finished(&self, before: Timestamp) -> Result<usize, StoreError> {
        self.db
            .run(move |c| {
                write(c, |tx| {
                    let jobs = {
                        let mut stmt = tx.prepare(
                            "SELECT job_id FROM runner_outbox o WHERE acked_at IS NOT NULL                              AND kind = 'job_result' AND created_at < ?1 AND seq =                              (SELECT MAX(seq) FROM runner_outbox m WHERE m.job_id = o.job_id)",
                        )?;
                        let rows = stmt.query_map([time(before)], |r| r.get::<_, String>(0))?;
                        rows.collect::<Result<Vec<_>, _>>()?
                    };
                    for job in &jobs {
                        tx.execute("DELETE FROM runner_outbox WHERE job_id = ?1", [job])?;
                        tx.execute("DELETE FROM runner_recv WHERE job_id = ?1", [job])?;
                    }
                    Ok(jobs.len())
                })
            })
            .await
    }

    async fn upsert_service(&self, s: ServiceRow) -> Result<(), StoreError> {
        self.db
            .run(move |c| {
                c.execute(
                    "INSERT INTO service_registry (name, pid, pid_start, state, started_at, \
                     expires_at, mem_mb) VALUES (?1,?2,?3,?4,?5,?6,?7) \
                     ON CONFLICT (name) DO UPDATE SET pid = excluded.pid, \
                     pid_start = excluded.pid_start, state = excluded.state, \
                     started_at = excluded.started_at, expires_at = excluded.expires_at, \
                     mem_mb = excluded.mem_mb",
                    params![
                        s.name,
                        s.pid,
                        opt_int(s.pid_start),
                        s.state.text(),
                        time(s.started_at),
                        time(s.expires_at),
                        int(s.mem_mb)
                    ],
                )?;
                Ok(())
            })
            .await
    }

    async fn services(&self) -> Result<Vec<ServiceRow>, StoreError> {
        self.db
            .run(|c| {
                let mut stmt = c.prepare(
                    "SELECT name, pid, pid_start, state, started_at, expires_at, mem_mb \
                     FROM service_registry ORDER BY name",
                )?;
                let rows = stmt
                    .query_map([], |r| {
                        Ok(ServiceRow {
                            name: r.get(0)?,
                            pid: r.get::<_, i64>(1)? as u32,
                            pid_start: col_opt_u64(r, 2)?,
                            state: col_enum(r, 3)?,
                            started_at: col_time(r, 4)?,
                            expires_at: col_time(r, 5)?,
                            mem_mb: col_u64(r, 6)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
    }

    async fn remove_service(&self, name: &str) -> Result<(), StoreError> {
        let name = name.to_string();
        self.db
            .run(move |c| {
                let n = c.execute("DELETE FROM service_registry WHERE name = ?1", [&name])?;
                if n == 0 {
                    return Err(not_found(format!("service {name}")));
                }
                Ok(())
            })
            .await
    }

    async fn upsert_cache(&self, e: CacheEntry) -> Result<(), StoreError> {
        self.db
            .run(move |c| {
                c.execute(
                    "INSERT INTO cache_index (client, project, name, path, bytes, last_used) \
                     VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT (client, project, name) DO UPDATE \
                     SET path = excluded.path, bytes = excluded.bytes, \
                     last_used = excluded.last_used",
                    params![
                        e.key.client,
                        e.key.project,
                        e.key.name,
                        e.path,
                        int(e.bytes),
                        time(e.last_used)
                    ],
                )?;
                Ok(())
            })
            .await
    }

    async fn cache_entries(&self) -> Result<Vec<CacheEntry>, StoreError> {
        self.db
            .run(|c| {
                let mut stmt = c.prepare(
                    "SELECT client, project, name, path, bytes, last_used FROM cache_index \
                     ORDER BY client, project, name",
                )?;
                let rows = stmt
                    .query_map([], |r| {
                        Ok(CacheEntry {
                            key: CacheKey {
                                client: r.get(0)?,
                                project: r.get(1)?,
                                name: r.get(2)?,
                            },
                            path: r.get(3)?,
                            bytes: col_u64(r, 4)?,
                            last_used: col_time(r, 5)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
    }

    async fn remove_cache(&self, key: &CacheKey) -> Result<(), StoreError> {
        let key = key.clone();
        self.db
            .run(move |c| {
                let n = c.execute(
                    "DELETE FROM cache_index WHERE client = ?1 AND project = ?2 AND name = ?3",
                    params![key.client, key.project, key.name],
                )?;
                if n == 0 {
                    return Err(not_found(format!(
                        "cache {}/{}/{}",
                        key.client, key.project, key.name
                    )));
                }
                Ok(())
            })
            .await
    }
}
