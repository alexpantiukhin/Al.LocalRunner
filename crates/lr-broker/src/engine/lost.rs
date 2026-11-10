use super::retry::Retry;
use super::start::StartWrite;
use super::{Cancel, Engine, Job};
use crate::policy::{OrphanQueued, OrphanRunning};
use lr_abstractions::{Applied, Transition};
use lr_core::{JobState, Timestamp};
use lr_proto::ReasonCode;
use std::collections::BTreeSet;

const BROKER: &str = "broker";

pub(crate) fn lost_deadline(job: &Job, grace_s: u64) -> Option<Timestamp> {
    let started = job.started_at?;
    let timeout_s = job.entry.order.requirements.timeout_s;
    Some(started.plus_secs(timeout_s).plus_secs(grace_s))
}

fn absence_cancel() -> Cancel {
    Cancel {
        by: BROKER.into(),
        reason: ReasonCode::HubAbsent.to_string(),
        code: ReasonCode::HubAbsent,
        detail: None,
        sent: false,
    }
}

impl Engine {
    pub(crate) async fn expire(&mut self) {
        let now = self.now();
        let grace_s = self.lost_kill_s();
        let due: Vec<usize> = (0..self.active.len())
            .filter(|&i| lost_deadline(&self.active[i], grace_s).is_some_and(|d| now >= d))
            .collect();
        let mut changed = !due.is_empty();
        for i in due.into_iter().rev() {
            let job = self.active.remove(i);
            self.lose(job, now).await;
        }
        if self.restored && now >= self.booted_at.plus_secs(grace_s) {
            self.restored = false;
            self.return_stranded().await;
            changed = true;
        }
        changed |= self.run_retries().await;
        changed |= self.orphans(now).await;
        if changed {
            self.plan().await;
        }
    }

    pub(crate) async fn lose(&mut self, mut job: Job, now: Timestamp) {
        if job.start_unsaved {
            if let StartWrite::Pending(e) = self.save_start(&mut job, false).await {
                return self.retry_later("mark a run lost", &e, Retry::Lose(job));
            }
        }
        let mut t = Transition::to(JobState::Lost, now);
        t.cancel_reason = Some(ReasonCode::RunnerLost.to_string());
        match self.store.transition(&job.key, t).await {
            Ok(Applied::Changed) => {}
            Ok(Applied::Ignored) => return,
            Err(e) => return self.retry_later("mark a run lost", &e, Retry::Lose(job)),
        }
        self.finish(
            &job.key,
            JobState::Lost,
            None,
            Some(ReasonCode::RunnerLost),
            job.runner
                .map(|r| format!("раннер {r} не отчитался в срок")),
        )
        .await;
    }

    async fn orphans(&mut self, now: Timestamp) -> bool {
        let orphans = self.policy.file().orphans.clone();
        let absent_ms = orphans.absent_after_min.get().saturating_mul(60_000);
        let clients: BTreeSet<String> = self
            .queue
            .iter()
            .chain(self.active.iter())
            .map(|j| j.key.client.clone())
            .filter(|k| !self.hubs.contains_key(k))
            .filter(|k| {
                let since = self.client_seen.get(k).copied().unwrap_or(self.booted_at);
                now.0 >= since.0.saturating_add(absent_ms)
            })
            .collect();
        if clients.is_empty() {
            return false;
        }
        let mut changed = false;
        if orphans.queued == OrphanQueued::Cancel {
            let (gone, keep): (Vec<Job>, Vec<Job>) = std::mem::take(&mut self.queue)
                .into_iter()
                .partition(|j| clients.contains(&j.key.client));
            self.queue = keep;
            changed |= !gone.is_empty();
            for job in gone {
                self.cancel_waiting(job, absence_cancel()).await;
            }
        }
        if orphans.running == OrphanRunning::Cancel {
            let targets: Vec<usize> = (0..self.active.len())
                .filter(|&i| {
                    let j = &self.active[i];
                    clients.contains(&j.key.client) && j.cancel.is_none()
                })
                .collect();
            for i in targets {
                self.mark_cancel(i, absence_cancel()).await;
            }
        }
        changed
    }
}
