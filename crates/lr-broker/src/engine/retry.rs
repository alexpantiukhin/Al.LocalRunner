use super::{Cancel, Engine, Job};
use lr_abstractions::{Level, StoreError};
use std::collections::HashSet;

const RETRY_AFTER_S: u64 = 1;

pub(crate) enum Retry {
    Requeue(Job),
    Cancel(Job, Cancel),
    Lose(Job),
}

impl Retry {
    fn job(&self) -> &Job {
        match self {
            Retry::Requeue(job) | Retry::Cancel(job, _) | Retry::Lose(job) => job,
        }
    }
}

impl Engine {
    pub(crate) fn retry_later(&mut self, what: &str, e: &StoreError, retry: Retry) {
        let key = retry.job().key.clone();
        self.diag(
            Level::Error,
            &format!("storage failed: could not {what}; retried in a second"),
            &[
                ("client", &key.client),
                ("job", &key.job.0),
                ("error", &e.to_string()),
            ],
        );
        if self.retries.is_empty() {
            self.retry_at = self.now().plus_secs(RETRY_AFTER_S);
        }
        self.retries.push(retry);
    }

    pub(crate) async fn run_retries(&mut self) -> bool {
        if self.retries.is_empty() || self.now() < self.retry_at {
            return false;
        }
        for retry in std::mem::take(&mut self.retries) {
            match retry {
                Retry::Requeue(job) => self.requeue_or_retry(job).await,
                Retry::Cancel(job, cancel) => self.cancel_waiting(job, cancel).await,
                Retry::Lose(job) => {
                    let now = self.now();
                    self.lose(job, now).await
                }
            }
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

    pub(crate) fn forget_clients(&mut self) {
        let mut busy: HashSet<&str> = self
            .queue
            .iter()
            .chain(self.active.iter())
            .chain(self.retries.iter().map(Retry::job))
            .map(|j| j.key.client.as_str())
            .collect();
        busy.extend(self.hubs.keys().map(String::as_str));
        self.client_seen.retain(|k, _| busy.contains(k.as_str()));
    }
}
