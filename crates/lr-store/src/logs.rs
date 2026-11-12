//! Job logs: full output in `logs/<client>/<job>.log` next to `broker.db` (spec 14), size and
//! last append in the `job_log` row.
//!
//! The row is the authority: its `bytes` is the log length. A crash between the file write and
//! the row update leaves extra bytes in the file, which the next append cuts off and a read
//! ignores. File data is handed to the OS, not fsynced, on every append.

use crate::codec::{col_u64, int, time};
use crate::db::{io_error, not_found, write, R};
use lr_abstractions::{JobKey, LogAppend, LOG_TRUNCATED_MARK};
use lr_core::Timestamp;
use rusqlite::{params, Connection, OptionalExtension};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const RESERVED: [&str; 4] = ["con", "prn", "aux", "nul"];

fn reserved(name: &str) -> bool {
    let numbered = |p: &str| {
        name.strip_prefix(p)
            .is_some_and(|n| n.len() == 1 && matches!(n.as_bytes()[0], b'1'..=b'9'))
    };
    RESERVED.contains(&name) || numbered("com") || numbered("lpt")
}

fn encode(part: &str) -> String {
    let mut out = String::new();
    for b in part.bytes() {
        if b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    if out.is_empty() {
        out.push_str("%00");
    }
    if reserved(&out) {
        out.insert(0, '_');
    }
    out
}

/// Path of the log relative to the database directory; every byte outside `[a-z0-9-]` of the
/// client key and job id is percent-encoded with upper-case hex, so no id can leave `logs/`,
/// two ids differing in case get different names on case-insensitive file systems, and a name
/// that is a Windows device name (`con`, `nul`, `com1`) gets a `_` prefix, which the encoding
/// never emits otherwise.
pub(crate) fn log_path(key: &JobKey) -> String {
    format!("logs/{}/{}.log", encode(&key.client), encode(&key.job.0))
}

fn exists(conn: &Connection, key: &JobKey) -> R<()> {
    conn.query_row(
        "SELECT 1 FROM job WHERE client_key = ?1 AND id = ?2",
        [&key.client, &key.job.0],
        |_| Ok(()),
    )
    .optional()?
    .ok_or_else(|| not_found(format!("job {}/{}", key.client, key.job)))
}

fn row(conn: &Connection, key: &JobKey) -> R<Option<(String, u64)>> {
    Ok(conn
        .query_row(
            "SELECT path, bytes FROM job_log WHERE client_key = ?1 AND job_id = ?2",
            [&key.client, &key.job.0],
            |r| Ok((r.get::<_, String>(0)?, col_u64(r, 1)?)),
        )
        .optional()?)
}

fn ends_with_mark(file: &mut std::fs::File, used: u64) -> std::io::Result<bool> {
    let mark = LOG_TRUNCATED_MARK.len() as u64;
    if used < mark {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(used - mark))?;
    let mut tail = vec![0u8; LOG_TRUNCATED_MARK.len()];
    file.read_exact(&mut tail)?;
    Ok(tail == LOG_TRUNCATED_MARK)
}

pub(crate) fn append(
    conn: &mut Connection,
    dir: &Path,
    key: &JobKey,
    data: &[u8],
    at: Timestamp,
    limit: u64,
) -> R<LogAppend> {
    write(conn, |tx| {
        exists(tx, key)?;
        let (rel, used) = row(tx, key)?.unwrap_or_else(|| (log_path(key), 0));
        let path = dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        if file.metadata()?.len() < used {
            return Err(io_error(format!(
                "log file {} is shorter than recorded",
                path.display()
            )));
        }
        if ends_with_mark(&mut file, used)? {
            return Ok(LogAppend::Dropped);
        }
        file.set_len(used)?;
        file.seek(SeekFrom::Start(used))?;
        let (result, stored) = if used + data.len() as u64 <= limit {
            file.write_all(data)?;
            (LogAppend::Appended, used + data.len() as u64)
        } else {
            let fits = limit.saturating_sub(used) as usize;
            file.write_all(&data[..fits])?;
            file.write_all(LOG_TRUNCATED_MARK)?;
            (
                LogAppend::Truncated,
                used + fits as u64 + LOG_TRUNCATED_MARK.len() as u64,
            )
        };
        tx.execute(
            "INSERT INTO job_log (client_key, job_id, path, bytes, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (client_key, job_id) DO UPDATE SET \
             bytes = excluded.bytes, updated_at = excluded.updated_at",
            params![key.client, key.job.0, rel, int(stored), time(at)],
        )?;
        Ok(result)
    })
}

pub(crate) fn read(conn: &Connection, dir: &Path, key: &JobKey) -> R<Vec<u8>> {
    exists(conn, key)?;
    let Some((rel, bytes)) = row(conn, key)? else {
        return Ok(Vec::new());
    };
    let path = dir.join(rel);
    let mut data =
        std::fs::read(&path).map_err(|e| io_error(format!("log file {}: {e}", path.display())))?;
    data.truncate(bytes as usize);
    Ok(data)
}

pub(crate) fn read_tail(conn: &Connection, dir: &Path, key: &JobKey, max: u64) -> R<Vec<u8>> {
    exists(conn, key)?;
    let Some((rel, bytes)) = row(conn, key)? else {
        return Ok(Vec::new());
    };
    let path = dir.join(rel);
    let fail = |e: std::io::Error| io_error(format!("log file {}: {e}", path.display()));
    let mut file = OpenOptions::new().read(true).open(&path).map_err(fail)?;
    let from = bytes.saturating_sub(max);
    file.seek(SeekFrom::Start(from)).map_err(fail)?;
    let mut data = Vec::new();
    file.take(bytes - from)
        .read_to_end(&mut data)
        .map_err(fail)?;
    Ok(data)
}
