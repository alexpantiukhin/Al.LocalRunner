use super::{Cancel, Engine, Job, Role};
use crate::policy::OrphanRunning;
use lr_abstractions::{Level, StoreError};
use lr_proto::ReasonCode;

const BROKER: &str = "broker";

fn removal_cancel() -> Cancel {
    Cancel {
        by: BROKER.into(),
        reason: ReasonCode::ClientRemoved.to_string(),
        code: ReasonCode::ClientRemoved,
        detail: None,
        sent: false,
    }
}

impl Engine {
    /// The key of machine `name` was revoked (spec 3.9, 15.5): its connections are closed,
    /// a job on one of its runners is `lost` with `runner_removed`, its waiting orders are
    /// cancelled with `client_removed`, its running orders follow `[orphans] running`, and its
    /// result outbox is deleted and stays empty.
    pub(crate) async fn revoke(&mut self, name: &str) {
        self.revoked.insert(name.to_string());
        let conns: Vec<_> = self
            .conns
            .iter()
            .filter(|(_, c)| match &c.role {
                Role::Hub(client) => client.machine.as_deref() == Some(name),
                Role::Runner(runner) => self
                    .runners
                    .get(runner)
                    .is_some_and(|r| r.persisted && r.owner == name),
                Role::Pending => false,
            })
            .map(|(id, _)| *id)
            .collect();
        for conn in conns {
            self.close(conn);
        }
        self.remove_runners_of(name).await;
        self.drop_orders_of(name).await;
        let now = self.now();
        match self.store.revoke(name, now).await {
            Ok(_) | Err(StoreError::NotFound(_)) => {}
            Err(e) => self.diag(
                Level::Error,
                "storage failed: the result outbox of a revoked key was not deleted",
                &[("machine", name), ("error", &e.to_string())],
            ),
        }
    }

    async fn remove_runners_of(&mut self, name: &str) {
        let removed: Vec<String> = self
            .runners
            .iter()
            .filter(|(_, r)| r.persisted && r.owner == name)
            .map(|(n, _)| n.clone())
            .collect();
        let now = self.now();
        let mut i = 0;
        while i < self.active.len() {
            let on_removed = self.active[i]
                .runner
                .as_ref()
                .is_some_and(|r| removed.contains(r));
            if on_removed {
                let job = self.active.remove(i);
                self.lose(job, now, ReasonCode::RunnerRemoved).await;
            } else {
                i += 1;
            }
        }
        for r in &removed {
            self.runners.remove(r);
            self.forget_runner(r);
        }
    }

    async fn drop_orders_of(&mut self, client: &str) {
        let (gone, keep): (Vec<Job>, Vec<Job>) = std::mem::take(&mut self.queue)
            .into_iter()
            .partition(|j| j.key.client == client);
        self.queue = keep;
        for job in gone {
            self.cancel_waiting(job, removal_cancel()).await;
        }
        if self.policy.file().orphans.running != OrphanRunning::Cancel {
            return;
        }
        let targets: Vec<usize> = (0..self.active.len())
            .filter(|&i| {
                let j = &self.active[i];
                j.key.client == client && j.cancel.is_none()
            })
            .collect();
        for i in targets {
            self.mark_cancel(i, removal_cancel()).await;
        }
    }

    /// Whether results for `client` may be stored: never for a revoked key.
    pub(crate) fn delivers_to(&self, client: &str) -> bool {
        !self.revoked.contains(client)
    }
}
