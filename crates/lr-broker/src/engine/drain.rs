use super::Engine;
use lr_abstractions::{Level, RunnerState};
use lr_proto::DrainReason;

impl Engine {
    pub(crate) async fn drain(&mut self, name: &str, reason: Option<DrainReason>) {
        let now = self.now();
        let Some(slot) = self.runners.get_mut(name) else {
            return;
        };
        if slot.draining {
            if slot.drain_reason.is_none() {
                slot.drain_reason = reason;
            }
            return;
        }
        slot.draining = true;
        slot.drain_reason = reason;
        let persisted = slot.persisted;
        if persisted {
            if let Err(e) = self
                .store
                .set_runner_state(name, RunnerState::Draining, now)
                .await
            {
                self.diag(
                    Level::Error,
                    "storage failed: could not record the runner drain",
                    &[("runner", name), ("error", &e.to_string())],
                );
            }
        }
        self.return_stranded().await;
        self.plan().await;
    }
}
