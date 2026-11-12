use super::retry::{transient, Retry};
use super::{Cancel, Engine, Job};
use lr_abstractions::{Applied, OutboxKind, StoreError, Transition};
use lr_core::{JobState, Timestamp};
use lr_proto::{BrokerToHub, JobResult, QueueEvent, StartedNotice};

pub(crate) enum StartWrite {
    Saved,
    Pending(StoreError),
    Refused,
}

fn cancelling(cancel: Option<&Cancel>, at: Timestamp) -> Transition {
    let mut t = Transition::to(JobState::Cancelling, at);
    if let Some(c) = cancel {
        t.cancelled_by = Some(c.by.clone());
        t.cancel_reason = Some(c.reason.clone());
    }
    t
}

impl Engine {
    pub(crate) async fn save_start(&mut self, job: &mut Job, with_cancel: bool) -> StartWrite {
        let at = job.started_at.unwrap_or(self.now());
        match self
            .store
            .transition(&job.key, Transition::to(JobState::Running, at))
            .await
        {
            Ok(Applied::Changed) => {}
            Ok(Applied::Ignored) => {
                job.start_unsaved = false;
                return StartWrite::Refused;
            }
            Err(e) if transient(&e) => return StartWrite::Pending(e),
            Err(e) => {
                self.store_failed(
                    "record the start of a run; the store refused it",
                    &job.key,
                    &e,
                );
                job.start_unsaved = false;
                return StartWrite::Refused;
            }
        }
        job.start_unsaved = false;
        if with_cancel && job.cancel.is_some() {
            let t = cancelling(job.cancel.as_ref(), self.now());
            if let Err(e) = self.store.transition(&job.key, t).await {
                self.store_failed("record the cancel of a started run", &job.key, &e);
            }
        }
        StartWrite::Saved
    }

    pub(crate) async fn save_unsaved_starts(&mut self) {
        let unsaved: Vec<usize> = (0..self.active.len())
            .filter(|&i| self.active[i].start_unsaved)
            .collect();
        for i in unsaved {
            let mut job = self.active[i].clone();
            self.save_start(&mut job, true).await;
            self.active[i].start_unsaved = job.start_unsaved;
        }
        self.unsaved_starts = self.active.iter().any(|j| j.start_unsaved);
    }

    pub(crate) async fn started(&mut self, i: usize, runner: &str, now: Timestamp) {
        if self.active[i].started_at.is_some() {
            return;
        }
        let mut job = self.active[i].clone();
        job.started_at = Some(now);
        match self.save_start(&mut job, true).await {
            StartWrite::Saved => {}
            StartWrite::Refused => return,
            StartWrite::Pending(e) => {
                if !self.retry_round {
                    self.store_failed(
                        "record the start of a run; retried in a second",
                        &job.key,
                        &e,
                    );
                }
                self.arm_retry();
                self.unsaved_starts = true;
                job.start_unsaved = true;
            }
        }
        let key = job.key.clone();
        self.active[i].started_at = Some(now);
        self.active[i].start_unsaved = job.start_unsaved;
        let msg = BrokerToHub::Started(StartedNotice {
            job: key.job.clone(),
            seq: 0,
            runner: runner.to_string(),
            started_at: now,
        });
        self.notify_hub(&key.client, &key.job, OutboxKind::Started, &msg)
            .await;
        self.publish(
            Some(&key.client),
            QueueEvent::JobStarted {
                job: key.job.clone(),
                runner: runner.to_string(),
            },
        );
    }

    fn park_result(&mut self, what: &str, e: &StoreError, job: Job, r: &JobResult, at: Timestamp) {
        self.retry_later(what, e, Retry::Result(job, Box::new(r.clone()), at));
    }

    pub(crate) async fn job_result(&mut self, mut job: Job, r: &JobResult, now: Timestamp) {
        if job.started_at.is_none() && r.state != JobState::Cancelled {
            job.started_at = Some(now);
            job.start_unsaved = true;
        }
        if job.start_unsaved {
            if let StartWrite::Pending(e) = self.save_start(&mut job, false).await {
                return self.park_result("record the start of a reported run", &e, job, r, now);
            }
        }
        let cancel = job
            .cancel
            .clone()
            .filter(|_| r.state == JobState::Cancelled);
        if r.state == JobState::Cancelled && job.started_at.is_some() {
            let running = match self.store.job(&job.key).await {
                Ok(rec) => rec.is_some_and(|rec| rec.job.state == JobState::Running),
                Err(e) => return self.park_result("read a cancelled run", &e, job, r, now),
            };
            if running {
                let t = cancelling(cancel.as_ref(), now);
                if let Err(e) = self.store.transition(&job.key, t).await {
                    return self.park_result("record the cancel of a run", &e, job, r, now);
                }
            }
        }
        let reason = r
            .reason
            .clone()
            .or_else(|| cancel.as_ref().map(|c| c.code.clone()));
        let mut t = Transition::to(r.state, now);
        t.exit_code = r.exit_code;
        t.peak_mem_mb = r.peak_mem_mb;
        t.peak_disk_mb = r.peak_disk_mb;
        match &cancel {
            Some(c) => {
                t.cancelled_by = Some(c.by.clone());
                t.cancel_reason = Some(c.reason.clone());
            }
            None => {
                t.cancel_reason = r.reason.as_ref().map(|code| match &r.detail {
                    Some(d) => format!("{code}: {d}"),
                    None => code.to_string(),
                });
            }
        }
        match self.store.transition(&job.key, t).await {
            Ok(Applied::Changed) => {
                let peaks = (r.peak_mem_mb, r.peak_disk_mb);
                self.finish_with_peaks(&job, r.state, r.exit_code, reason, r.detail.clone(), peaks)
                    .await
            }
            Ok(Applied::Ignored) => {}
            Err(e) => self.park_result("record the result of a run", &e, job, r, now),
        }
    }
}
