use crate::codec::{col_opt_enum, col_opt_u64, col_u64, int, opt_int, Text};
use crate::db::{io_error, not_found, write, Db, Fail, R};
use crate::outbox::{self, HUB};
use crate::schema;
use crate::seq::decide;
use async_trait::async_trait;
use lr_abstractions::{ActiveOrder, BrokerLink, HubStore, OutboxEntry, OutboxKind, StoreError};
use lr_core::{JobId, Timestamp, Zone};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::any::Any;
use std::io::Write;
use std::path::{Path, PathBuf};

/// `hub.db` on SQLite: the outbox of orders and cancels, the active orders and the broker
/// list, plus the log copies in the submitter directories.
pub struct SqliteHubStore {
    db: Db,
}

impl SqliteHubStore {
    /// Opens `hub.db` at `path`, creating the file and the schema when missing.
    pub fn open(path: &Path) -> Result<SqliteHubStore, StoreError> {
        Ok(SqliteHubStore {
            db: Db::open(path, &schema::HUB, true)?,
        })
    }

    /// Like [`SqliteHubStore::open`] but fails when the file does not exist.
    pub fn open_existing(path: &Path) -> Result<SqliteHubStore, StoreError> {
        Ok(SqliteHubStore {
            db: Db::open(path, &schema::HUB, false)?,
        })
    }

    /// Closes the connection and opens the same file again, as after a restart.
    pub fn reopen(self) -> Result<SqliteHubStore, StoreError> {
        Ok(SqliteHubStore {
            db: self.db.reopen()?,
        })
    }

    /// Ties `guard` (a temporary directory, say) to the life of this handle.
    pub fn keep_alive(self, guard: impl Any + Send + Sync) -> SqliteHubStore {
        SqliteHubStore {
            db: self.db.keep_alive(guard),
        }
    }

    /// The database file.
    pub fn path(&self) -> &Path {
        self.db.path()
    }
}

const ACTIVE: &str = "job_id, broker, last_seq, label, os_user, submitter_pid, submitter_start, \
                      handle_path, detached, spec_json";
const ID_FLOOR: &str = "job_id_floor";

fn active_row(r: &Row<'_>) -> rusqlite::Result<ActiveOrder> {
    Ok(ActiveOrder {
        job: JobId(r.get(0)?),
        broker: r.get(1)?,
        last_seq: col_u64(r, 2)?,
        label: r.get(3)?,
        os_user: r.get(4)?,
        submitter_pid: r.get::<_, i64>(5)? as u32,
        submitter_start: col_opt_u64(r, 6)?,
        handle_path: r.get(7)?,
        detached: r.get(8)?,
        spec_json: r.get(9)?,
    })
}

fn require_active(conn: &Connection, job: &JobId) -> R<()> {
    conn.query_row(
        "SELECT 1 FROM active_order WHERE job_id = ?1",
        [&job.0],
        |_| Ok(()),
    )
    .optional()?
    .map(|_| ())
    .ok_or_else(|| not_found(format!("active order {job}")))
}

fn append_copy(path: &Path, data: &[u8]) -> R<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| io_error(format!("{}: {e}", path.display())))?;
    file.write_all(data)?;
    Ok(())
}

#[async_trait]
impl HubStore for SqliteHubStore {
    async fn enqueue(
        &self,
        order: ActiveOrder,
        kind: OutboxKind,
        payload: String,
        at: Timestamp,
    ) -> Result<u64, StoreError> {
        self.db
            .run(move |c| {
                write(c, |tx| {
                    let taken = tx
                        .query_row(
                            "SELECT 1 FROM active_order WHERE job_id = ?1",
                            [&order.job.0],
                            |_| Ok(()),
                        )
                        .optional()?;
                    if taken.is_some() {
                        return Err(Fail::Store(StoreError::Duplicate(format!(
                            "active order {}",
                            order.job
                        ))));
                    }
                    tx.execute(
                        &format!(
                            "INSERT INTO active_order ({ACTIVE}) \
                             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"
                        ),
                        params![
                            order.job.0,
                            order.broker,
                            int(order.last_seq),
                            order.label,
                            order.os_user,
                            order.submitter_pid,
                            opt_int(order.submitter_start),
                            order.handle_path,
                            order.detached,
                            order.spec_json
                        ],
                    )?;
                    outbox::push(tx, HUB, "", &order.job, kind, &payload, at)
                })
            })
            .await
    }

    async fn active(&self, job: &JobId) -> Result<Option<ActiveOrder>, StoreError> {
        let job = job.clone();
        self.db
            .run(move |c| {
                Ok(c.query_row(
                    &format!("SELECT {ACTIVE} FROM active_order WHERE job_id = ?1"),
                    [&job.0],
                    active_row,
                )
                .optional()?)
            })
            .await
    }

    async fn active_orders(&self) -> Result<Vec<ActiveOrder>, StoreError> {
        self.db
            .run(|c| {
                let mut stmt = c.prepare(&format!(
                    "SELECT {ACTIVE} FROM active_order ORDER BY job_id"
                ))?;
                let rows = stmt
                    .query_map([], active_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
    }

    async fn set_broker(&self, job: &JobId, broker: Option<&str>) -> Result<(), StoreError> {
        let (job, broker) = (job.clone(), broker.map(str::to_string));
        self.db
            .run(move |c| {
                let n = c.execute(
                    "UPDATE active_order SET broker = ?2 WHERE job_id = ?1",
                    params![job.0, broker],
                )?;
                if n == 0 {
                    return Err(not_found(format!("active order {job}")));
                }
                Ok(())
            })
            .await
    }

    async fn accept_seq(&self, job: &JobId, seq: u64) -> Result<bool, StoreError> {
        let job = job.clone();
        self.db
            .run(move |c| {
                write(c, |tx| {
                    let last: i64 = tx
                        .query_row(
                            "SELECT last_seq FROM active_order WHERE job_id = ?1",
                            [&job.0],
                            |r| r.get(0),
                        )
                        .optional()?
                        .ok_or_else(|| not_found(format!("active order {job}")))?;
                    let next = decide(last as u64, seq)?;
                    if next {
                        tx.execute(
                            "UPDATE active_order SET last_seq = ?2 WHERE job_id = ?1",
                            params![job.0, int(seq)],
                        )?;
                    }
                    Ok(next)
                })
            })
            .await
    }

    async fn finish(&self, job: &JobId) -> Result<(), StoreError> {
        let job = job.clone();
        self.db
            .run(move |c| {
                let n = c.execute("DELETE FROM active_order WHERE job_id = ?1", [&job.0])?;
                if n == 0 {
                    return Err(not_found(format!("active order {job}")));
                }
                Ok(())
            })
            .await
    }

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
                    require_active(tx, &job)?;
                    outbox::push(tx, HUB, "", &job, kind, &payload, at)
                })
            })
            .await
    }

    async fn pending_outbox(&self) -> Result<Vec<OutboxEntry>, StoreError> {
        self.db.run(|c| outbox::pending(c, HUB, "")).await
    }

    async fn ack_outbox(&self, job: &JobId, seq: u64) -> Result<usize, StoreError> {
        let job = job.clone();
        self.db
            .run(move |c| write(c, |tx| outbox::close(tx, HUB, "", Some(&job), Some(seq))))
            .await
    }

    async fn discard_outbox(&self, job: &JobId) -> Result<usize, StoreError> {
        let job = job.clone();
        self.db
            .run(move |c| write(c, |tx| outbox::close(tx, HUB, "", Some(&job), None)))
            .await
    }

    async fn rebroker(
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
                    require_active(tx, &job)?;
                    tx.execute(
                        "UPDATE active_order SET broker = NULL, last_seq = 0 WHERE job_id = ?1",
                        [&job.0],
                    )?;
                    tx.execute("DELETE FROM outbox WHERE job_id = ?1", [&job.0])?;
                    outbox::push(tx, HUB, "", &job, kind, &payload, at)
                })
            })
            .await
    }

    async fn job_id_floor(&self) -> Result<u64, StoreError> {
        self.db
            .run(|c| {
                let v: Option<i64> = c
                    .query_row(
                        "SELECT value FROM hub_meta WHERE name = ?1",
                        [ID_FLOOR],
                        |r| r.get(0),
                    )
                    .optional()?;
                Ok(v.map_or(0, |v| v as u64))
            })
            .await
    }

    async fn raise_job_id_floor(&self, ms: u64) -> Result<(), StoreError> {
        self.db
            .run(move |c| {
                write(c, |tx| {
                    tx.execute(
                        "INSERT INTO hub_meta (name, value) VALUES (?1, ?2)                          ON CONFLICT (name) DO UPDATE SET value = max(value, excluded.value)",
                        params![ID_FLOOR, int(ms)],
                    )?;
                    Ok(())
                })
            })
            .await
    }

    async fn upsert_broker(&self, link: BrokerLink) -> Result<(), StoreError> {
        self.db
            .run(move |c| {
                c.execute(
                    "INSERT INTO broker_link (broker, address, fingerprint, key_id, machine_id, \
                     last_zone) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT (broker) DO UPDATE SET \
                     address = excluded.address, fingerprint = excluded.fingerprint, \
                     key_id = excluded.key_id, machine_id = excluded.machine_id, \
                     last_zone = excluded.last_zone",
                    params![
                        link.name,
                        link.address,
                        link.fingerprint,
                        link.key_id,
                        link.machine_id,
                        link.last_zone.map(|z| z.text())
                    ],
                )?;
                Ok(())
            })
            .await
    }

    async fn brokers(&self) -> Result<Vec<BrokerLink>, StoreError> {
        self.db
            .run(|c| {
                let mut stmt = c.prepare(
                    "SELECT broker, address, fingerprint, key_id, machine_id, last_zone \
                     FROM broker_link ORDER BY broker",
                )?;
                let rows = stmt
                    .query_map([], |r| {
                        Ok(BrokerLink {
                            name: r.get(0)?,
                            address: r.get(1)?,
                            fingerprint: r.get(2)?,
                            key_id: r.get(3)?,
                            machine_id: r.get(4)?,
                            last_zone: col_opt_enum::<Zone>(r, 5)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
    }

    async fn remove_broker(&self, name: &str) -> Result<(), StoreError> {
        let name = name.to_string();
        self.db
            .run(move |c| {
                let n = c.execute("DELETE FROM broker_link WHERE broker = ?1", [&name])?;
                if n == 0 {
                    return Err(not_found(format!("broker {name}")));
                }
                Ok(())
            })
            .await
    }

    async fn set_zone(&self, name: &str, zone: Zone) -> Result<(), StoreError> {
        let name = name.to_string();
        self.db
            .run(move |c| {
                let n = c.execute(
                    "UPDATE broker_link SET last_zone = ?2 WHERE broker = ?1",
                    params![name, zone.text()],
                )?;
                if n == 0 {
                    return Err(not_found(format!("broker {name}")));
                }
                Ok(())
            })
            .await
    }

    async fn append_log_copy(&self, path: &Path, data: &[u8]) -> Result<(), StoreError> {
        let (path, data): (PathBuf, Vec<u8>) = (path.to_path_buf(), data.to_vec());
        self.db.run(move |_| append_copy(&path, &data)).await
    }
}
