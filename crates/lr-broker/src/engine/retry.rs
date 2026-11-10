use super::{Cancel, Engine, Job};
use lr_abstractions::{JobKey, Level, StoreError};
use lr_core::Timestamp;
use lr_proto::JobResult;
use std::collections::HashSet;

const RETRY_AFTER_S: u64 = 1;

pub(crate) enum Retry {
    Requeue(Job),
    Cancel(Job, Cancel),
    Lose(Job),
    Result(Job, Box<JobResult>, Timestamp),
}

impl Retry {
    fn key(&self) -> &JobKey {
        match self {
            Retry::Requeue(job)
            | Retry::Cancel(job, _)
            | Retry::Lose(job)
            | Retry::Result(job, _, _) => &job.key,
        }
    }
}

pub(crate) fn transient(e: &StoreError) -> bool {
    matches!(e, StoreError::Io(_))
}

impl Engine {
    pub(crate) fn store_failed(&self, what: &str, key: &JobKey, e: &StoreError) {
        self.diag(
            Level::Error,
            &format!("storage failed: could not {what}"),
            &[
                ("client", &key.client),
                ("job", &key.job.0),
                ("error", &e.to_string()),
            ],
        );
    }

    pub(crate) fn arm_retry(&mut self) {
        if self.retries.is_empty() && !self.unsaved_starts {
            self.retry_at = self.now().plus_secs(RETRY_AFTER_S);
        }
    }

    pub(crate) fn retry_later(&mut self, what: &str, e: &StoreError, retry: Retry) {
        if !transient(e) {
            return self.store_failed(&format!("{what}; the store refused it"), retry.key(), e);
        }
        if !self.retry_round {
            let key = retry.key().clone();
            self.store_failed(&format!("{what}; retried in a second"), &key, e);
        }
        self.arm_retry();
        self.retries.push(retry);
    }

    pub(crate) async fn run_retries(&mut self) -> bool {
        if (self.retries.is_empty() && !self.unsaved_starts) || self.now() < self.retry_at {
            return false;
        }
        self.retry_round = true;
        self.save_unsaved_starts().await;
        for retry in std::mem::take(&mut self.retries) {
            match retry {
                Retry::Requeue(job) => self.requeue_or_retry(job).await,
                Retry::Cancel(job, cancel) => self.cancel_waiting(job, cancel).await,
                Retry::Lose(job) => {
                    let now = self.now();
                    self.lose(job, now).await
                }
                Retry::Result(job, r, at) => self.job_result(job, &r, at).await,
            }
        }
        self.retry_round = false;
        let left = self.retries.len() + self.active.iter().filter(|j| j.start_unsaved).count();
        if left > 0 {
            self.retry_at = self.now().plus_secs(RETRY_AFTER_S);
            self.diag(
                Level::Error,
                &format!("storage failed: {left} writes still failing; retried in a second"),
                &[("pending", &left.to_string())],
            );
        }
        true
    }

    pub(crate) async fn requeue_or_retry(&mut self, mut job: Job) {
        if let Err(e) = self.store.requeue(&job.key).await {
            return self.retry_later("put a run back into the queue", &e, Retry::Requeue(job));
        }
        job.runner = None;
        match job.cancel.take() {
            Some(cancel) => self.cancel_waiting(job, cancel).await,
            None => self.queue.push(job),
        }
    }

    pub(crate) fn cancel_parked(&mut self, key: &JobKey, cancel: &Cancel) -> Option<bool> {
        let retry = self.retries.iter_mut().find(|r| r.key() == key)?;
        match retry {
            Retry::Requeue(job) if job.cancel.is_none() => {
                job.cancel = Some(cancel.clone());
                Some(true)
            }
            _ => Some(false),
        }
    }

    pub(crate) fn parked_keys(&self) -> impl Iterator<Item = &JobKey> {
        self.retries.iter().map(Retry::key)
    }

    pub(crate) fn forget_clients(&mut self) {
        let mut busy: HashSet<&str> = self
            .queue
            .iter()
            .chain(self.active.iter())
            .map(|j| j.key.client.as_str())
            .collect();
        busy.extend(self.retries.iter().map(|r| r.key().client.as_str()));
        busy.extend(self.hubs.keys().map(String::as_str));
        self.client_seen.retain(|k, _| busy.contains(k.as_str()));
    }
}
