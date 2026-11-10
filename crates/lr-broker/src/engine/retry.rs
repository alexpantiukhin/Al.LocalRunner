use super::{Cancel, Engine, Job};
use lr_abstractions::{Applied, JobKey, Level, StoreError, Transition};
use lr_core::{JobState, Timestamp};
use std::collections::HashSet;

const RETRY_AFTER_S: u64 = 1;

pub(crate) enum Retry {
    Requeue(Job),
    Cancel(Job, Cancel),
    Lose(Job),
    Start(JobKey, Timestamp),
}

impl Retry {
    fn key(&self) -> &JobKey {
        match self {
            Retry::Requeue(job) | Retry::Cancel(job, _) | Retry::Lose(job) => &job.key,
            Retry::Start(key, _) => key,
        }
    }
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

    pub(crate) async fn settle_start(&mut self, job: &Job) {
        if !self.take_pending_start(&job.key) {
            return;
        }
        let at = job.started_at.unwrap_or(self.now());
        if let Err(e) = self
            .store
            .transition(&job.key, Transition::to(JobState::Running, at))
            .await
        {
            self.store_failed("record the start of a run", &job.key, &e);
        }
    }

    pub(crate) fn retry_later(&mut self, what: &str, e: &StoreError, retry: Retry) {
        if !self.retry_round {
            let key = retry.key().clone();
            self.diag(
                Level::Error,
                &format!("storage failed: could not {what}; retried in a second"),
                &[
                    ("client", &key.client),
                    ("job", &key.job.0),
                    ("error", &e.to_string()),
                ],
            );
        }
        if self.retries.is_empty() {
            self.retry_at = self.now().plus_secs(RETRY_AFTER_S);
        }
        self.retries.push(retry);
    }

    pub(crate) async fn run_retries(&mut self) -> bool {
        if self.retries.is_empty() || self.now() < self.retry_at {
            return false;
        }
        self.retry_round = true;
        for retry in std::mem::take(&mut self.retries) {
            match retry {
                Retry::Requeue(job) => self.requeue_or_retry(job).await,
                Retry::Cancel(job, cancel) => self.cancel_waiting(job, cancel).await,
                Retry::Lose(job) => {
                    let now = self.now();
                    self.lose(job, now).await
                }
                Retry::Start(key, at) => {
                    self.write_start(key, at).await;
                }
            }
        }
        self.retry_round = false;
        if !self.retries.is_empty() {
            let left = self.retries.len().to_string();
            self.diag(
                Level::Error,
                &format!("storage failed: {left} writes still failing; retried in a second"),
                &[("pending", &left)],
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

    pub(crate) async fn write_start(&mut self, key: JobKey, at: Timestamp) -> bool {
        match self
            .store
            .transition(&key, Transition::to(JobState::Running, at))
            .await
        {
            Ok(applied) => applied == Applied::Changed,
            Err(e) => {
                self.retry_later("record the start of a run", &e, Retry::Start(key, at));
                true
            }
        }
    }

    pub(crate) fn take_pending_start(&mut self, key: &JobKey) -> bool {
        let before = self.retries.len();
        self.retries
            .retain(|r| !matches!(r, Retry::Start(k, _) if k == key));
        self.retries.len() != before
    }

    pub(crate) fn cancel_parked(&mut self, key: &JobKey, cancel: &Cancel) -> Option<bool> {
        let retry = self
            .retries
            .iter_mut()
            .find(|r| !matches!(r, Retry::Start(..)) && r.key() == key)?;
        match retry {
            Retry::Requeue(job) if job.cancel.is_none() => {
                job.cancel = Some(cancel.clone());
                Some(true)
            }
            _ => Some(false),
        }
    }

    pub(crate) fn parked_keys(&self) -> impl Iterator<Item = &JobKey> {
        self.retries
            .iter()
            .filter(|r| !matches!(r, Retry::Start(..)))
            .map(Retry::key)
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
