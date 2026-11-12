//! History retention (spec 8): logs by age and total size, rows by age, oldest first, final
//! runs only. Database rows go in one transaction; the files follow once it is committed, so a
//! crash leaves at worst an orphan file, never a row pointing at nothing.

use crate::codec::final_list;
use crate::db::{write, R};
use lr_abstractions::{PruneLimits, PruneReport};
use rusqlite::{params_from_iter, Connection, Transaction};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const ORPHAN_GRACE_MS: u64 = 24 * 60 * 60 * 1000;

struct Doomed {
    client: String,
    machine: String,
    job: String,
    path: Option<String>,
    bytes: u64,
}

fn clamp(v: u64) -> i64 {
    v.min(i64::MAX as u64) as i64
}

fn select(tx: &Transaction<'_>, extra: &str, args: &[i64]) -> R<Vec<Doomed>> {
    let sql = format!(
        "SELECT j.client_key, j.id, l.path, l.bytes, j.client_machine FROM job j \
         LEFT JOIN job_log l ON l.client_key = j.client_key AND l.job_id = j.id \
         WHERE j.state IN {} \
         AND j.finished_at IS NOT NULL {extra} \
         ORDER BY j.finished_at, j.client_key, j.id",
        final_list()
    );
    let mut stmt = tx.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(args.iter()), |r| {
            let bytes: Option<i64> = r.get(3)?;
            Ok(Doomed {
                client: r.get(0)?,
                machine: r.get(4)?,
                job: r.get(1)?,
                path: r.get(2)?,
                bytes: bytes.unwrap_or(0) as u64,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

struct Work {
    report: PruneReport,
    files: Vec<String>,
}

impl Work {
    fn drop_log(&mut self, tx: &Transaction<'_>, d: &Doomed) -> R<()> {
        if let Some(path) = &d.path {
            tx.execute(
                "DELETE FROM job_log WHERE client_key = ?1 AND job_id = ?2",
                [&d.client, &d.job],
            )?;
            self.report.logs_removed += 1;
            self.report.bytes_freed += d.bytes;
            self.files.push(path.clone());
        }
        Ok(())
    }
}

pub(crate) fn prune(conn: &mut Connection, dir: &Path, limits: PruneLimits) -> R<PruneReport> {
    let work = write(conn, |tx| {
        let mut work = Work {
            report: PruneReport::default(),
            files: Vec::new(),
        };
        let now = clamp(limits.now.0);
        for d in select(
            tx,
            "AND ?1 - j.finished_at > ?2",
            &[now, clamp(limits.row_max_age_ms)],
        )? {
            work.drop_log(tx, &d)?;
            tx.execute(
                "DELETE FROM job WHERE client_key = ?1 AND id = ?2",
                [&d.client, &d.job],
            )?;
            tx.execute(
                "DELETE FROM result_outbox WHERE client_machine = ?1 AND job_id = ?2 \
                 AND acked_at IS NOT NULL AND NOT EXISTS \
                 (SELECT 1 FROM job WHERE client_machine = ?1 AND id = ?2)",
                [&d.machine, &d.job],
            )?;
            work.report.rows_removed += 1;
        }
        for d in select(
            tx,
            "AND l.path IS NOT NULL AND ?1 - j.finished_at > ?2",
            &[now, clamp(limits.log_max_age_ms)],
        )? {
            work.drop_log(tx, &d)?;
        }
        let mut total: i64 =
            tx.query_row("SELECT COALESCE(SUM(bytes), 0) FROM job_log", [], |r| {
                r.get(0)
            })?;
        for d in select(tx, "AND l.path IS NOT NULL", &[])? {
            if total <= clamp(limits.log_max_bytes) {
                break;
            }
            total -= d.bytes as i64;
            work.drop_log(tx, &d)?;
        }
        Ok(work)
    })?;
    let mut report = work.report;
    let mut failed = HashSet::new();
    for file in &work.files {
        let path = dir.join(file);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                report.files_failed += 1;
                failed.insert(path.clone());
            }
        }
        remove_empty_parent(&path);
    }
    sweep_orphans(conn, dir, limits, &failed, &mut report)?;
    Ok(report)
}

fn remove_empty_parent(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

fn modified_ms(path: &Path) -> Option<u64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    let ms = t.duration_since(UNIX_EPOCH).ok()?.as_millis();
    Some(u64::try_from(ms).unwrap_or(u64::MAX))
}

fn sweep_orphans(
    conn: &Connection,
    dir: &Path,
    limits: PruneLimits,
    failed: &HashSet<PathBuf>,
    report: &mut PruneReport,
) -> R<()> {
    let known: HashSet<PathBuf> = {
        let mut stmt = conn.prepare("SELECT path FROM job_log")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|p| dir.join(p))
            .collect()
    };
    let Ok(clients) = std::fs::read_dir(dir.join("logs")) else {
        return Ok(());
    };
    for client in clients.flatten() {
        let Ok(files) = std::fs::read_dir(client.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if known.contains(&path) || failed.contains(&path) {
                continue;
            }
            let old = modified_ms(&path)
                .is_some_and(|m| limits.now.0.saturating_sub(m) > ORPHAN_GRACE_MS);
            if !old {
                continue;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => report.orphans_removed += 1,
                Err(_) => report.files_failed += 1,
            }
        }
        remove_empty_parent(&client.path().join("x"));
    }
    Ok(())
}
