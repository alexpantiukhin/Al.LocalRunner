use super::conn::ConnId;
use super::retry::Retry;
use super::{Cancel, Client, Engine, Job};
use crate::policy::CancelOthers;
use lr_abstractions::{Applied, Inbound, JobKey, OutboxKind, StoreError, Transition};
use lr_core::JobState;
use lr_proto::{
    Ack, BrokerToHub, BrokerToRunner, CancelAck, CancelOutcome, HubCancel, ProtocolErrorCode,
    RunnerCancel,
};

fn ack_of(c: &HubCancel, outcome: CancelOutcome, denial: Option<String>) -> BrokerToHub {
    BrokerToHub::CancelAck(CancelAck {
        job: c.job.clone(),
        seq: 0,
        cancel_seq: c.seq,
        outcome,
        denial,
    })
}

impl Engine {
    pub(crate) async fn cancel(&mut self, conn: ConnId, client: &Client, c: HubCancel) {
        let ack = BrokerToHub::Ack(Ack {
            job: c.job.clone(),
            seq: c.seq,
        });
        if c.client.as_deref().is_none_or(|k| k == client.key) {
            let own = JobKey {
                client: client.key.clone(),
                job: c.job.clone(),
            };
            match self.store.job(&own).await {
                Ok(Some(rec)) => {
                    return self.cancel_own(conn, client, &own, rec.job.state, c).await
                }
                Ok(None) => {}
                Err(e) => return self.unavailable(conn, &own, "read the job", &e),
            }
        }
        self.send(conn, &ack);
        let matches: Vec<JobKey> = self
            .queue
            .iter()
            .chain(self.active.iter())
            .map(|j| &j.key)
            .chain(self.parked_keys())
            .filter(|k| k.job == c.job && k.client != client.key)
            .filter(|k| c.client.as_deref().is_none_or(|w| w == k.client))
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let allowed = client.owner && self.policy.file().cancel.others == CancelOthers::Owner;
        let denial = match matches.as_slice() {
            [] => Some(format!("заказ {} не найден", c.job)),
            _ if !allowed => Some(
                "чужой заказ: отменять чужие заказы может только владелец брокера \
                 ([cancel] others)"
                    .to_string(),
            ),
            [_] => None,
            many => Some(format!(
                "JobId {} есть у нескольких клиентов ({}); укажите клиента",
                c.job,
                many.iter()
                    .map(|k| k.client.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        };
        match denial {
            Some(why) => self.send(conn, &ack_of(&c, CancelOutcome::Denied, Some(why))),
            None => {
                self.apply_cancel(&matches[0], &c).await;
                let msg = ack_of(&c, CancelOutcome::Applied, None);
                self.notify_hub(&client.key, &c.job, OutboxKind::CancelAck, &msg)
                    .await;
            }
        }
    }

    async fn cancel_own(
        &mut self,
        conn: ConnId,
        client: &Client,
        own: &JobKey,
        state: JobState,
        c: HubCancel,
    ) {
        let ack = BrokerToHub::Ack(Ack {
            job: c.job.clone(),
            seq: c.seq,
        });
        match self.store.accept_seq(own, Inbound::Hub, c.seq).await {
            Ok(true) => {}
            Ok(false) => return self.send(conn, &ack),
            Err(StoreError::SeqGap { expected, got }) => {
                return self.error(
                    conn,
                    ProtocolErrorCode::SeqGap,
                    format!("job {}: expected seq {expected}, got {got}", c.job),
                )
            }
            Err(e) => return self.unavailable(conn, own, "record the cancel", &e),
        }
        self.send(conn, &ack);
        let outcome = if state.is_final() {
            CancelOutcome::AlreadyFinal
        } else {
            self.apply_cancel(own, &c).await;
            CancelOutcome::Applied
        };
        let msg = ack_of(&c, outcome, None);
        self.notify_hub(&client.key, &c.job, OutboxKind::CancelAck, &msg)
            .await;
    }

    async fn apply_cancel(&mut self, key: &JobKey, c: &HubCancel) {
        let cancel = Cancel {
            by: c.by.label.clone(),
            reason: match &c.reason {
                Some(r) => format!("{}: {r}", c.code),
                None => c.code.to_string(),
            },
            code: c.code.clone(),
            detail: c.reason.clone(),
            sent: false,
        };
        if let Some(i) = self.queue.iter().position(|j| &j.key == key) {
            let job = self.queue.remove(i);
            self.cancel_waiting(job, cancel).await;
            return self.plan().await;
        }
        if let Some(fresh) = self.cancel_parked(key, &cancel) {
            if fresh {
                if let Err(e) = self
                    .store
                    .record_cancel(key, &cancel.by, &cancel.reason)
                    .await
                {
                    self.store_failed("record a cancel; it holds in memory only", key, &e);
                }
            }
            return;
        }
        let Some(i) = self.active.iter().position(|j| &j.key == key) else {
            return;
        };
        if self.active[i].cancel.is_some() {
            return;
        }
        self.mark_cancel(i, cancel).await;
    }

    pub(crate) async fn mark_cancel(&mut self, i: usize, cancel: Cancel) {
        let key = self.active[i].key.clone();
        self.active[i].cancel = Some(cancel.clone());
        let written = if self.active[i].started_at.is_some() && !self.active[i].start_unsaved {
            let mut t = Transition::to(JobState::Cancelling, self.now());
            t.cancelled_by = Some(cancel.by);
            t.cancel_reason = Some(cancel.reason);
            self.store.transition(&key, t).await
        } else {
            self.store
                .record_cancel(&key, &cancel.by, &cancel.reason)
                .await
        };
        if let Err(e) = written {
            self.store_failed(
                "record a cancel; it holds in memory only and is lost on a restart",
                &key,
                &e,
            );
        }
        self.send_cancel(i).await;
    }

    pub(crate) async fn send_cancel(&mut self, i: usize) {
        let job = &self.active[i];
        let Some(cancel) = job.cancel.as_ref().filter(|c| !c.sent) else {
            return;
        };
        let Some(conn) = job.runner.as_deref().and_then(|r| self.runner_conn(r)) else {
            return;
        };
        let (key, code, detail) = (job.key.clone(), cancel.code.clone(), cancel.detail.clone());
        let Ok(seq) = self.store.next_seq_to_runner(&key).await else {
            return;
        };
        self.send(
            conn,
            &BrokerToRunner::Cancel(RunnerCancel {
                job: key.job.clone(),
                seq,
                code,
                detail,
            }),
        );
        if let Some(c) = self.active[i].cancel.as_mut() {
            c.sent = true;
        }
    }

    pub(crate) async fn cancel_waiting(&mut self, job: Job, cancel: Cancel) {
        let mut t = Transition::to(JobState::Cancelled, self.now());
        t.cancelled_by = Some(cancel.by.clone());
        t.cancel_reason = Some(cancel.reason.clone());
        match self.store.transition(&job.key, t).await {
            Ok(applied) => {
                self.waits.remove(&job.key);
                if applied == Applied::Changed {
                    self.finish(
                        &job.key,
                        JobState::Cancelled,
                        None,
                        Some(cancel.code),
                        cancel.detail,
                    )
                    .await;
                }
            }
            Err(e) => self.retry_later("cancel a waiting run", &e, Retry::Cancel(job, cancel)),
        }
    }
}
