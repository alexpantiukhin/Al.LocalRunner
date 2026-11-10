//! The outbox mechanism shared by `hub.db`, `runner.db` and the broker's result outbox
//! (spec 15.3): seq counted per job (and per recipient on the broker) from 1, never reused.
//!
//! An acknowledged or discarded entry is not deleted at once: it is closed (`acked_at` set,
//! payload emptied) and only the closed entry with the greatest seq of its job stays, so the
//! next seq is `max(seq) + 1` even when nothing is pending. A closed entry carries no time:
//! the store never reads a clock, so `acked_at` holds `created_at` as a marker.

use crate::codec::{col_enum, col_time, col_u64, int, time, Text};
use crate::db::R;
use lr_abstractions::{OutboxEntry, OutboxKind};
use lr_core::{JobId, Timestamp};
use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection};
use std::collections::HashMap;

#[derive(Clone, Copy)]
pub(crate) struct Table {
    pub(crate) name: &'static str,
    pub(crate) scope: Option<&'static str>,
}

pub(crate) const HUB: Table = Table {
    name: "outbox",
    scope: None,
};
pub(crate) const RUNNER: Table = Table {
    name: "runner_outbox",
    scope: None,
};
pub(crate) const RESULTS: Table = Table {
    name: "result_outbox",
    scope: Some("client_machine"),
};

struct Filter {
    sql: String,
    args: Vec<Value>,
}

impl Table {
    fn filter(&self, scope: &str, job: Option<&str>) -> Filter {
        let mut f = Filter {
            sql: "1 = 1".to_string(),
            args: Vec::new(),
        };
        if let Some(col) = self.scope {
            f.args.push(Value::Text(scope.to_string()));
            f.sql.push_str(&format!(" AND {col} = ?{}", f.args.len()));
        }
        if let Some(job) = job {
            f.args.push(Value::Text(job.to_string()));
            f.sql.push_str(&format!(" AND job_id = ?{}", f.args.len()));
        }
        f
    }
}

pub(crate) fn push(
    conn: &Connection,
    t: Table,
    scope: &str,
    job: &JobId,
    kind: OutboxKind,
    payload: &str,
    at: Timestamp,
) -> R<u64> {
    let f = t.filter(scope, Some(&job.0));
    let sql = format!(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM {} WHERE {}",
        t.name, f.sql
    );
    let seq: i64 = conn.query_row(&sql, params_from_iter(f.args.iter()), |r| r.get(0))?;
    let (cols, marks) = match t.scope {
        Some(col) => (format!("{col}, "), "?6, "),
        None => (String::new(), ""),
    };
    let sql = format!(
        "INSERT INTO {} ({cols}job_id, seq, kind, payload_json, created_at) \
         VALUES ({marks}?1, ?2, ?3, ?4, ?5)",
        t.name
    );
    let mut args: Vec<Value> = vec![
        Value::Text(job.0.clone()),
        Value::Integer(seq),
        Value::Text(kind.text().to_string()),
        Value::Text(payload.to_string()),
        Value::Integer(time(at)),
    ];
    if t.scope.is_some() {
        args.push(Value::Text(scope.to_string()));
    }
    conn.execute(&sql, params_from_iter(args.iter()))?;
    Ok(seq as u64)
}

pub(crate) fn pending_bytes(conn: &Connection, t: Table, scope: &str) -> R<u64> {
    let f = t.filter(scope, None);
    let sql = format!(
        "SELECT COALESCE(SUM(length(CAST(payload_json AS BLOB))), 0) FROM {} \
         WHERE acked_at IS NULL AND seq > 0 AND {}",
        t.name, f.sql
    );
    let n: i64 = conn.query_row(&sql, params_from_iter(f.args.iter()), |r| r.get(0))?;
    Ok(n as u64)
}

/// Unacknowledged entries of one recipient (or of everyone when the table has no scope), the
/// job with the oldest pending entry first, by seq within a job.
pub(crate) fn pending(conn: &Connection, t: Table, scope: &str) -> R<Vec<OutboxEntry>> {
    let f = t.filter(scope, None);
    let sql = format!(
        "SELECT job_id, seq, kind, payload_json, created_at FROM {} \
         WHERE acked_at IS NULL AND seq > 0 AND {}",
        t.name, f.sql
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(f.args.iter()), |r| {
        Ok(OutboxEntry {
            job: JobId(r.get(0)?),
            seq: col_u64(r, 1)?,
            kind: col_enum(r, 2)?,
            payload: r.get(3)?,
            created_at: col_time(r, 4)?,
        })
    })?;
    let mut out = rows.collect::<Result<Vec<_>, _>>()?;
    let mut first: HashMap<JobId, Timestamp> = HashMap::new();
    for e in &out {
        let at = first.entry(e.job.clone()).or_insert(e.created_at);
        *at = (*at).min(e.created_at);
    }
    out.sort_by(|a, b| (first[&a.job], &a.job, a.seq).cmp(&(first[&b.job], &b.job, b.seq)));
    Ok(out)
}

/// Closes pending entries: of `job` up to `upto` when given, of the job when `upto` is `None`,
/// of the whole table (or recipient) when `job` is `None`. Returns how many were pending.
pub(crate) fn close(
    conn: &Connection,
    t: Table,
    scope: &str,
    job: Option<&JobId>,
    upto: Option<u64>,
) -> R<usize> {
    let mut f = t.filter(scope, job.map(|j| j.0.as_str()));
    if let Some(upto) = upto {
        f.args.push(Value::Integer(int(upto)));
        f.sql.push_str(&format!(" AND seq <= ?{}", f.args.len()));
    }
    let sql = format!(
        "UPDATE {} SET acked_at = created_at, payload_json = '' \
         WHERE acked_at IS NULL AND seq > 0 AND {}",
        t.name, f.sql
    );
    let closed = conn.execute(&sql, params_from_iter(f.args.iter()))?;
    compact(conn, t)?;
    Ok(closed)
}

fn compact(conn: &Connection, t: Table) -> R<()> {
    let same = match t.scope {
        Some(col) => format!("AND newer.{col} = {0}.{col}", t.name),
        None => String::new(),
    };
    conn.execute(
        &format!(
            "DELETE FROM {0} WHERE acked_at IS NOT NULL AND seq > 0 AND seq < \
             (SELECT MAX(newer.seq) FROM {0} AS newer WHERE newer.job_id = {0}.job_id {same})",
            t.name
        ),
        [],
    )?;
    Ok(())
}

/// Deletes every entry of a recipient, closed ones included; returns how many were pending.
pub(crate) fn drop_recipient(conn: &Connection, t: Table, scope: &str) -> R<usize> {
    let f = t.filter(scope, None);
    let sql = format!(
        "SELECT count(*) FROM {} WHERE acked_at IS NULL AND seq > 0 AND {}",
        t.name, f.sql
    );
    let n: i64 = conn.query_row(&sql, params_from_iter(f.args.iter()), |r| r.get(0))?;
    conn.execute(
        &format!("DELETE FROM {} WHERE {}", t.name, f.sql),
        params_from_iter(f.args.iter()),
    )?;
    Ok(n as usize)
}

/// Deletes entries created before `before` and returns how many of them were pending.
pub(crate) fn expire(conn: &Connection, t: Table, before: Timestamp) -> R<usize> {
    let n: i64 = conn.query_row(
        &format!(
            "SELECT count(*) FROM {} WHERE acked_at IS NULL AND seq > 0 AND created_at < ?1",
            t.name
        ),
        [time(before)],
        |r| r.get(0),
    )?;
    conn.execute(
        &format!("DELETE FROM {} WHERE seq > 0 AND created_at < ?1", t.name),
        [time(before)],
    )?;
    Ok(n as usize)
}
