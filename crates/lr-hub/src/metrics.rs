//! Metrics of the hub (spec 12.2): the size and age of its outbox, sampled every
//! [`OUTBOX_SAMPLE_S`] seconds.

use crate::engine::Engine;
use lr_abstractions::metric::{OUTBOX_OLDEST_AGE_SECONDS, OUTBOX_PENDING, OUTBOX_SAMPLE_S, ROLE};
use lr_abstractions::Level;

impl Engine {
    pub(crate) async fn sample_outbox(&mut self) {
        let now = self.now();
        if now < self.next_outbox_sample {
            return;
        }
        self.next_outbox_sample = now.plus_secs(OUTBOX_SAMPLE_S);
        match self.s.store.outbox_backlog().await {
            Ok(b) => {
                let labels = [(ROLE, "hub")];
                self.s.meter.set(OUTBOX_PENDING, &labels, b.pending as f64);
                let age = b
                    .oldest
                    .map_or(0.0, |t| now.millis_since(t) as f64 / 1000.0);
                self.s.meter.set(OUTBOX_OLDEST_AGE_SECONDS, &labels, age);
            }
            Err(e) => self.s.diag(
                Level::Warn,
                "outbox not measured",
                &[("error", &e.to_string())],
            ),
        }
    }
}
