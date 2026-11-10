//! Join codes, key records and runners of `broker.db` (spec 3.8, 15.4).

use crate::codec::{
    col_enum, col_json, col_opt_time, col_opt_u64, col_time, col_u32, col_u64, int, json, opt_int,
    opt_time, time, StringSet, Text,
};
use crate::db::{not_found, write, Fail, R};
use crate::outbox::{self, RESULTS};
use lr_abstractions::{Join, JoinCodeRow, MachineRow, Revoked, RunnerRow, RunnerState, StoreError};
use lr_core::Timestamp;
use rusqlite::{params, Connection, OptionalExtension, Row};

const MACHINE: &str = "name, machine_id, kind, label, public_key, registered_at, last_seen, \
                       revoked_at";
const CODE: &str = "code_hash, kind, name, label, uses_left, issued_at, expires_at, redeemed_at, \
                    machine_name";
const RUNNER: &str = "name, machine_name, programs, platforms, tags, max_timeout_s, cache_max_mb, \
                      cache_used_mb, free_disk_mb, services, accept, last_seen, state";

fn machine_row(r: &Row<'_>) -> rusqlite::Result<MachineRow> {
    Ok(MachineRow {
        name: r.get(0)?,
        machine_id: r.get(1)?,
        kind: col_enum(r, 2)?,
        label: r.get(3)?,
        public_key: r.get(4)?,
        registered_at: col_time(r, 5)?,
        last_seen: col_opt_time(r, 6)?,
        revoked_at: col_opt_time(r, 7)?,
    })
}

fn code_row(r: &Row<'_>) -> rusqlite::Result<JoinCodeRow> {
    Ok(JoinCodeRow {
        code_hash: r.get(0)?,
        kind: col_enum(r, 1)?,
        name: r.get(2)?,
        label: r.get(3)?,
        uses_left: col_u32(r, 4)?,
        issued_at: col_time(r, 5)?,
        expires_at: col_time(r, 6)?,
        redeemed_at: col_opt_time(r, 7)?,
        machine_name: r.get(8)?,
    })
}

fn accept_min_mem(r: &Row<'_>, idx: usize) -> rusqlite::Result<Option<u64>> {
    let text: Option<String> = r.get(idx)?;
    Ok(text
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["min_mem_mb"].as_u64()))
}

fn runner_row(r: &Row<'_>) -> rusqlite::Result<RunnerRow> {
    Ok(RunnerRow {
        name: r.get(0)?,
        machine_name: r.get(1)?,
        programs: col_json(r, 2)?,
        platforms: col_json(r, 3)?,
        tags: col_json(r, 4)?,
        max_timeout_s: col_opt_u64(r, 5)?,
        cache_max_mb: col_u64(r, 6)?,
        cache_used_mb: col_u64(r, 7)?,
        free_disk_mb: col_u64(r, 8)?,
        services: col_json(r, 9)?,
        accept_min_mem_mb: accept_min_mem(r, 10)?,
        last_seen: col_opt_time(r, 11)?,
        state: col_enum(r, 12)?,
    })
}

pub(crate) fn issue_code(conn: &mut Connection, c: JoinCodeRow) -> R<()> {
    write(conn, |tx| {
        let taken = tx
            .query_row(
                "SELECT 1 FROM join_code WHERE code_hash = ?1",
                [&c.code_hash],
                |_| Ok(()),
            )
            .optional()?;
        if taken.is_some() {
            return Err(Fail::Store(StoreError::Duplicate("join code".into())));
        }
        tx.execute(
            &format!("INSERT INTO join_code ({CODE}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"),
            params![
                c.code_hash,
                c.kind.text(),
                c.name,
                c.label,
                c.uses_left,
                time(c.issued_at),
                time(c.expires_at),
                opt_time(c.redeemed_at),
                c.machine_name
            ],
        )?;
        Ok(())
    })
}

pub(crate) fn join_code(conn: &Connection, hash: &str) -> R<Option<JoinCodeRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {CODE} FROM join_code WHERE code_hash = ?1"),
            [hash],
            code_row,
        )
        .optional()?)
}

pub(crate) fn join_codes(conn: &Connection) -> R<Vec<JoinCodeRow>> {
    let mut stmt = conn.prepare(&format!("SELECT {CODE} FROM join_code ORDER BY code_hash"))?;
    let rows = stmt
        .query_map([], code_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(crate) fn redeem(conn: &mut Connection, join: Join) -> R<MachineRow> {
    write(conn, |tx| {
        let code = tx
            .query_row(
                &format!("SELECT {CODE} FROM join_code WHERE code_hash = ?1"),
                [&join.code_hash],
                code_row,
            )
            .optional()?
            .ok_or(Fail::Store(StoreError::CodeUnknown))?;
        let refuse = |e: StoreError| Err(Fail::Store(e));
        if join.at >= code.expires_at {
            return refuse(StoreError::CodeExpired);
        }
        if code.uses_left == 0 {
            return refuse(StoreError::CodeUsedUp);
        }
        if code.kind != join.machine.kind {
            return refuse(StoreError::CodeKindMismatch);
        }
        if code.name.as_ref().is_some_and(|n| *n != join.machine.name) {
            return refuse(StoreError::CodeNameMismatch);
        }
        let mut m = join.machine;
        if let Some(label) = &code.label {
            m.label = label.clone();
        }
        let taken = tx
            .query_row(
                "SELECT 1 FROM machine WHERE revoked_at IS NULL AND (name = ?1 OR public_key = ?2)",
                [&m.name, &m.public_key],
                |_| Ok(()),
            )
            .optional()?;
        if taken.is_some() {
            return refuse(StoreError::Duplicate(format!("machine {}", m.name)));
        }
        tx.execute(
            &format!(
                "INSERT INTO machine ({MACHINE}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8) \
                 ON CONFLICT (name) DO UPDATE SET machine_id = excluded.machine_id, \
                 kind = excluded.kind, label = excluded.label, public_key = excluded.public_key, \
                 registered_at = excluded.registered_at, last_seen = excluded.last_seen, \
                 revoked_at = excluded.revoked_at"
            ),
            params![
                m.name,
                m.machine_id,
                m.kind.text(),
                m.label,
                m.public_key,
                time(m.registered_at),
                opt_time(m.last_seen),
                opt_time(m.revoked_at)
            ],
        )?;
        let left = code.uses_left - 1;
        tx.execute(
            "UPDATE join_code SET uses_left = ?2, machine_name = ?3, \
             redeemed_at = CASE WHEN ?2 = 0 THEN ?4 ELSE redeemed_at END WHERE code_hash = ?1",
            params![join.code_hash, left, m.name, time(join.at)],
        )?;
        Ok(m)
    })
}

pub(crate) fn machines(conn: &Connection) -> R<Vec<MachineRow>> {
    let mut stmt = conn.prepare(&format!("SELECT {MACHINE} FROM machine ORDER BY name"))?;
    let rows = stmt
        .query_map([], machine_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(crate) fn machine_by_key(conn: &Connection, key: &str) -> R<Option<MachineRow>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {MACHINE} FROM machine WHERE public_key = ?1 \
                 ORDER BY revoked_at IS NOT NULL, name LIMIT 1"
            ),
            [key],
            machine_row,
        )
        .optional()?)
}

pub(crate) fn touch_machine(conn: &mut Connection, name: &str, at: Timestamp) -> R<()> {
    let n = conn.execute(
        "UPDATE machine SET last_seen = ?2 WHERE name = ?1",
        params![name, time(at)],
    )?;
    if n == 0 {
        return Err(not_found(format!("machine {name}")));
    }
    Ok(())
}

pub(crate) fn revoke(conn: &mut Connection, name: &str, at: Timestamp) -> R<Revoked> {
    write(conn, |tx| {
        let n = tx.execute(
            "UPDATE machine SET revoked_at = COALESCE(revoked_at, ?2) WHERE name = ?1",
            params![name, time(at)],
        )?;
        if n == 0 {
            return Err(not_found(format!("machine {name}")));
        }
        let mut stmt = tx.prepare(
            "SELECT name FROM runner WHERE machine_name = ?1 AND state != 'removed' ORDER BY name",
        )?;
        let runners = stmt
            .query_map([name], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.execute(
            "UPDATE runner SET state = 'removed' WHERE machine_name = ?1",
            [name],
        )?;
        let outbox_removed = outbox::close(tx, RESULTS, name, None, None)?;
        Ok(Revoked {
            runners,
            outbox_removed,
        })
    })
}

pub(crate) fn upsert_runner(conn: &mut Connection, r: RunnerRow) -> R<()> {
    write(conn, |tx| {
        let machine = tx
            .query_row(
                "SELECT 1 FROM machine WHERE name = ?1",
                [&r.machine_name],
                |_| Ok(()),
            )
            .optional()?;
        if machine.is_none() {
            return Err(not_found(format!("machine {}", r.machine_name)));
        }
        let old: Option<(String, RunnerState)> = tx
            .query_row(
                "SELECT machine_name, state FROM runner WHERE name = ?1",
                [&r.name],
                |row| Ok((row.get(0)?, col_enum(row, 1)?)),
            )
            .optional()?;
        if let Some((owner, state)) = old {
            if owner != r.machine_name && state != RunnerState::Removed {
                return Err(Fail::Store(StoreError::Duplicate(format!(
                    "runner {}",
                    r.name
                ))));
            }
        }
        let accept = r
            .accept_min_mem_mb
            .map(|m| json(&serde_json::json!({ "min_mem_mb": m })));
        tx.execute(
            &format!(
                "INSERT INTO runner ({RUNNER}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13) \
                 ON CONFLICT (name) DO UPDATE SET machine_name = excluded.machine_name, \
                 programs = excluded.programs, platforms = excluded.platforms, \
                 tags = excluded.tags, max_timeout_s = excluded.max_timeout_s, \
                 cache_max_mb = excluded.cache_max_mb, cache_used_mb = excluded.cache_used_mb, \
                 free_disk_mb = excluded.free_disk_mb, services = excluded.services, \
                 accept = excluded.accept, last_seen = excluded.last_seen, state = excluded.state"
            ),
            params![
                r.name,
                r.machine_name,
                json::<StringSet>(&r.programs),
                json::<StringSet>(&r.platforms),
                json::<StringSet>(&r.tags),
                opt_int(r.max_timeout_s),
                int(r.cache_max_mb),
                int(r.cache_used_mb),
                int(r.free_disk_mb),
                json(&r.services),
                accept,
                opt_time(r.last_seen),
                r.state.text()
            ],
        )?;
        Ok(())
    })
}

pub(crate) fn set_runner_state(
    conn: &mut Connection,
    name: &str,
    state: RunnerState,
    at: Timestamp,
) -> R<()> {
    let n = conn.execute(
        "UPDATE runner SET state = ?2, last_seen = ?3 WHERE name = ?1",
        params![name, state.text(), time(at)],
    )?;
    if n == 0 {
        return Err(not_found(format!("runner {name}")));
    }
    Ok(())
}

pub(crate) fn runners(conn: &Connection) -> R<Vec<RunnerRow>> {
    let mut stmt = conn.prepare(&format!("SELECT {RUNNER} FROM runner ORDER BY name"))?;
    let rows = stmt
        .query_map([], runner_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
