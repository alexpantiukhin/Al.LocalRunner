use super::{Engine, Job};
use lr_abstractions::{Applied, Level, OutboxKind, Transition};
use lr_core::{JobState, Zone};
use lr_proto::{BrokerToHub, QueueEvent, ReasonCode, RejectReason, Returned};

const NO_RUNNER_HINT: &str = "нет раннера на вашей установке, запустите его: up";

impl Engine {
    fn has_live_runner(&self, job: &Job) -> bool {
        let Some(machine) = &job.entry.order.requirements.same_machine else {
            return true;
        };
        self.runners.values().any(|s| {
            s.takes_work()
                && s.caps.machine.as_ref() == Some(machine)
                && self
                    .site
                    .zone_between(&job.entry.client.endpoint, &s.endpoint)
                    == Zone::Local
        })
    }

    pub(crate) async fn return_stranded(&mut self) {
        if self.restored {
            return;
        }
        let (gone, keep): (Vec<Job>, Vec<Job>) = std::mem::take(&mut self.queue)
            .into_iter()
            .partition(|j| !self.has_live_runner(j));
        self.queue = keep;
        for job in gone {
            if let Some(job) = self.give_back(job).await {
                self.queue.push(job);
            }
        }
    }

    async fn give_back(&mut self, job: Job) -> Option<Job> {
        let code = ReasonCode::Rejected(RejectReason::NoRunnerOnMachine);
        let mut t = Transition::to(JobState::Cancelled, self.now());
        t.cancelled_by = Some("broker".into());
        t.cancel_reason = Some(format!("{code}: {NO_RUNNER_HINT}"));
        match self.store.transition(&job.key, t).await {
            Ok(Applied::Changed) => {}
            Ok(Applied::Ignored) => return None,
            Err(e) => {
                self.diag(
                    Level::Error,
                    "storage failed: could not return a same-machine order; it stays queued",
                    &[
                        ("client", &job.key.client),
                        ("job", &job.key.job.0),
                        ("error", &e.to_string()),
                    ],
                );
                return Some(job);
            }
        }
        self.waits.remove(&job.key);
        self.wait_codes.remove(&job.key);
        self.count_end(&job, JobState::Cancelled);
        let msg = BrokerToHub::Returned(Returned {
            job: job.key.job.clone(),
            seq: 0,
            code: code.clone(),
            detail: Some(NO_RUNNER_HINT.into()),
        });
        self.notify_hub(&job.key.client, &job.key.job, OutboxKind::JobResult, &msg)
            .await;
        self.publish(
            Some(&job.key.client),
            QueueEvent::JobFinished {
                job: job.key.job.clone(),
                state: JobState::Cancelled,
                reason: Some(code),
            },
        );
        self.end_follows(&job.key);
        None
    }
}
