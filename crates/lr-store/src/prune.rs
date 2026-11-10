//! History retention (spec 8): logs by age and total size, rows by age, oldest first, final
//! runs only. Database rows go in one transaction; the files follow once it is committed, so a
//! crash leaves at worst an orphan file, never a row pointing at nothing.

use crate::db::{write, R};
use lr_abstractions::{PruneLimits, PruneReport};
use rusqlite::{params_from_iter, Connection, Transaction};
use std::path::Path;

struct Doomed {
    client: String,
    job: String,
    path: Option<String>,
    bytes: u64,
}

fn clamp(v: u64) -> i64 {
    v.min(i64::MAX as u64) as i64
}

fn select(tx: &Transaction<'_>, extra: &str, args: &[i64]) -> R<Vec<Doomed>> {
    let sql = format!(
        "SELECT j.client_key, j.id, l.path, l.bytes FROM job j \
         LEFT JOIN job_log l ON l.client_key = j.client_key AND l.job_id = j.id \
         WHERE j.state IN ('passed', 'failed', 'timeout', 'cancelled', 'lost') \
         AND j.finished_at IS NOT NULL {extra} \
         ORDER BY j.finished_at, j.client_key, j.id"
    );
    let mut stmt = tx.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(args.iter()), |r| {
            let bytes: Option<i64> = r.get(3)?;
            Ok(Doomed {
                client: r.get(0)?,
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
    for file in &work.files {
        let path = dir.join(file);
        let _ = std::fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
    Ok(work.report)
}
