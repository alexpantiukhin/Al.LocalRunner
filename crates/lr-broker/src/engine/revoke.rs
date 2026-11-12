use super::{Cancel, Engine, Job, Role};
use crate::policy::OrphanRunning;
use lr_abstractions::{JobKey, Level, StoreError};
use lr_core::JobId;
use lr_proto::ReasonCode;
use std::collections::HashSet;

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
    /// a job on one of its runners is `lost` with `runner_removed`, the runs of the key are
    /// retired (see [`Engine::retire`]) and its result outbox is deleted.
    pub(crate) async fn revoke(&mut self, name: &str) {
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
        let keys: HashSet<JobKey> = self
            .queue
            .iter()
            .chain(self.active.iter())
            .map(|j| &j.key)
            .chain(self.parked_keys())
            .filter(|k| k.client == name)
            .cloned()
            .collect();
        self.retire(keys).await;
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

    /// Runs of a revoked key: never reported to any hub again, waiting ones cancelled with
    /// `client_removed`, running ones as `[orphans] running` says.
    pub(crate) async fn retire(&mut self, keys: HashSet<JobKey>) {
        if keys.is_empty() {
            return;
        }
        self.silenced.extend(keys.iter().cloned());
        let (gone, keep): (Vec<Job>, Vec<Job>) = std::mem::take(&mut self.queue)
            .into_iter()
            .partition(|j| keys.contains(&j.key));
        self.queue = keep;
        for job in gone {
            self.cancel_waiting(job, removal_cancel()).await;
        }
        for key in &keys {
            self.cancel_parked(key, &removal_cancel());
        }
        if self.policy.file().orphans.running != OrphanRunning::Cancel {
            return;
        }
        let targets: Vec<usize> = (0..self.active.len())
            .filter(|&i| {
                let j = &self.active[i];
                keys.contains(&j.key) && j.cancel.is_none()
            })
            .collect();
        for i in targets {
            self.mark_cancel(i, removal_cancel()).await;
        }
    }

    pub(crate) fn delivers_to(&self, client: &str, job: &JobId) -> bool {
        self.silenced.is_empty()
            || !self.silenced.contains(&JobKey {
                client: client.to_string(),
                job: job.clone(),
            })
    }
}
