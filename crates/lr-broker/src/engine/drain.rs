use super::Engine;
use lr_abstractions::{Level, RunnerState};

impl Engine {
    pub(crate) async fn drain(&mut self, name: &str) {
        let now = self.now();
        let Some(slot) = self.runners.get_mut(name) else {
            return;
        };
        if slot.draining {
            return;
        }
        slot.draining = true;
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
        self.plan().await;
    }
}
