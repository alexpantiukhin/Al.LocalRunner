use super::{Client, Engine};
use lr_abstractions::JobKey;
use lr_proto::{ReasonCode, Withdraw};

const MOVED_HINT: &str = "the hub places the order at another broker where it starts now";

impl Engine {
    /// The hub asks for a not yet started order back. The engine handles one message at a
    /// time, so the order is either still queued, and goes back, or already assigned, and
    /// stays: the answer to a late withdraw is the `Started` that follows anyway.
    pub(crate) async fn withdraw(&mut self, client: &Client, w: Withdraw) {
        let key = JobKey {
            client: client.key.clone(),
            job: w.job,
        };
        let Some(i) = self.queue.iter().position(|j| j.key == key) else {
            return;
        };
        let job = self.queue.remove(i);
        if let Some(job) = self.give_back(job, ReasonCode::Moved, MOVED_HINT).await {
            self.queue.insert(i, job);
            return;
        }
        self.plan().await;
    }
}
