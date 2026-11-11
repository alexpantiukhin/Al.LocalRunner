//! Bounds of the hub's queues (spec 13.3) and a count of how often each was reached; every
//! time is written to the diagnostic log with the running `total`.

use crate::engine::Shared;
use lr_abstractions::Level;
use std::sync::atomic::{AtomicU64, Ordering};

/// Frames queued to one broker connection.
pub(crate) const OUT_FRAMES: usize = 1024;
/// Replies queued to one CLI connection.
pub(crate) const REPLY_QUEUE: usize = 256;
/// Results of finished jobs kept in memory for `wait` and `jobs`.
pub(crate) const RECENT_MAX: usize = 1024;

#[derive(Clone, Copy)]
pub(crate) enum Overflow {
    BrokerQueue,
    BrokerDeferred,
    CliReply,
    Watcher,
    Result,
}

#[derive(Default)]
pub(crate) struct Overflows([AtomicU64; 5]);

impl Shared {
    pub(crate) fn overflow(
        &self,
        kind: Overflow,
        level: Level,
        message: &str,
        fields: &[(&str, &str)],
    ) {
        let total = self.overflows.0[kind as usize].fetch_add(1, Ordering::Relaxed) + 1;
        let total = total.to_string();
        let mut all = fields.to_vec();
        all.push(("total", &total));
        self.diag(level, message, &all);
    }
}
