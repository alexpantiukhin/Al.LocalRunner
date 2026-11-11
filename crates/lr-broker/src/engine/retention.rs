//! Retention (spec 8, 14): results kept for an absent hub `[results] keep_hours`, logs and
//! rows of final runs by `[history]`. The store work runs off the engine task, one pass at a
//! time, so a long prune never holds the queue.

use super::Engine;
use lr_abstractions::{Level, LogSink, PruneLimits, Record, Role};
use lr_core::Timestamp;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Seconds between two retention passes.
pub(crate) const RETENTION_EVERY_S: u64 = 600;

const HOUR_MS: u64 = 3_600_000;
const DAY_MS: u64 = 24 * HOUR_MS;
const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct Retention {
    next: Timestamp,
    busy: Arc<AtomicBool>,
}

fn warn(log: &dyn LogSink, at: Timestamp, message: &str, field: (&str, &str)) {
    log.write(
        &Record::new(at, Level::Warn, Role::Broker, "engine", message).field(field.0, field.1),
    );
}

impl Engine {
    pub(crate) fn retain(&mut self) {
        let now = self.now();
        if now < self.retention.next || self.retention.busy.load(Ordering::Acquire) {
            return;
        }
        self.retention.next = now.plus_secs(RETENTION_EVERY_S);
        self.retention.busy.store(true, Ordering::Release);
        let file = self.policy.file();
        let keep_ms = file.results.keep_hours.get().saturating_mul(HOUR_MS);
        let expire_before = Timestamp(now.0.saturating_sub(keep_ms));
        let limits = PruneLimits {
            now,
            log_max_age_ms: file.history.keep_days.get().saturating_mul(DAY_MS),
            log_max_bytes: file.history.logs_max_gb.get().saturating_mul(GIB),
            row_max_age_ms: file.history.rows_keep_days.get().saturating_mul(DAY_MS),
        };
        let (store, log, busy) = (
            self.store.clone(),
            self.log.clone(),
            self.retention.busy.clone(),
        );
        tokio::spawn(async move {
            if let Err(e) = store.expire_results(expire_before).await {
                let msg = "storage failed: expired results were not removed";
                warn(log.as_ref(), now, msg, ("error", &e.to_string()));
            }
            match store.prune(limits).await {
                Err(e) => warn(
                    log.as_ref(),
                    now,
                    "storage failed: history was not pruned",
                    ("error", &e.to_string()),
                ),
                Ok(r) if r.files_failed > 0 => warn(
                    log.as_ref(),
                    now,
                    "some log files could not be removed; the next pass retries",
                    ("files", &r.files_failed.to_string()),
                ),
                Ok(_) => {}
            }
            busy.store(false, Ordering::Release);
        });
    }
}
