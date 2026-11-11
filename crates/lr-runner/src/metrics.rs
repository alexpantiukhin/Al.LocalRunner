//! Metrics of the runner (spec 12.2) besides the cache ones: the size and age of its outbox.

use crate::agent::Shared;
use lr_abstractions::metric::{OUTBOX_OLDEST_AGE_SECONDS, OUTBOX_PENDING, ROLE};
use lr_abstractions::Level;

impl Shared {
    pub(crate) async fn sample_outbox(&self) {
        match self.deps.store.outbox_backlog().await {
            Ok(b) => {
                let labels = [(ROLE, "runner")];
                let meter = &self.deps.meter;
                meter.set(OUTBOX_PENDING, &labels, b.pending as f64);
                let age = b
                    .oldest
                    .map_or(0.0, |t| self.now().millis_since(t) as f64 / 1000.0);
                meter.set(OUTBOX_OLDEST_AGE_SECONDS, &labels, age);
            }
            Err(e) => self.diag.event(
                Level::Warn,
                "outbox not measured",
                &[("error", &e.to_string())],
            ),
        }
    }
}
