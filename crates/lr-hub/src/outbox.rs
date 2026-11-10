//! Sending the outbox (spec 13.3): entries go to the broker of their job with their stored
//! seq, are sent again after every reconnect until `accepted` or `ack` removes them, and an
//! order no broker accepted within `ttl_hours` ends as `rejected_outbox_expired`.

use crate::engine::Engine;
use crate::policy::OnExpire;
use lr_abstractions::{Frame, Level, OutboxEntry, OutboxKind};
use lr_core::{Identity, JobId};
use lr_proto::{HubCancel, HubToBroker, ReasonCode, RejectReason};
use tokio::sync::mpsc::error::TrySendError;

fn wire(e: &OutboxEntry) -> Result<Frame, String> {
    let mut msg: HubToBroker = serde_json::from_str(&e.payload).map_err(|x| x.to_string())?;
    match &mut msg {
        HubToBroker::Order(o) => o.seq = e.seq,
        HubToBroker::Cancel(c) => c.seq = e.seq,
        _ => return Err("outbox entry is neither an order nor a cancel".into()),
    }
    Frame::json(&msg).map_err(|x| x.to_string())
}

impl Engine {
    /// Sends what is pending for `name` and not yet sent on its current connection; a full
    /// send queue leaves the rest for the next tick.
    pub(crate) async fn flush(&mut self, name: &str) {
        let Some(out) = self.links.get(name).and_then(|l| l.out.clone()) else {
            return;
        };
        let pending = match self.s.store.pending_outbox().await {
            Ok(p) => p,
            Err(e) => {
                self.s.diag(
                    Level::Error,
                    "outbox unreadable",
                    &[("error", &e.to_string())],
                );
                return;
            }
        };
        let Some(link) = self.links.get_mut(name) else {
            return;
        };
        link.flush_later = false;
        for e in pending {
            let ours = self
                .tracks
                .get(&e.job)
                .is_some_and(|t| t.order.broker.as_deref() == Some(name));
            let key = (e.job.clone(), e.seq);
            if !ours || link.sent.contains(&key) {
                continue;
            }
            let frame = match wire(&e) {
                Ok(f) => f,
                Err(why) => {
                    self.s.diag(
                        Level::Error,
                        "outbox entry unreadable, not sent",
                        &[("job", &e.job.0), ("error", &why)],
                    );
                    continue;
                }
            };
            match out.try_send(frame) {
                Ok(()) => {
                    link.sent.insert(key);
                }
                Err(TrySendError::Full(_)) => {
                    link.flush_later = true;
                    return;
                }
                Err(TrySendError::Closed(_)) => return,
            }
        }
    }

    /// Stores a cancel for the job's broker and sends it.
    pub(crate) async fn push_cancel(
        &mut self,
        job: &JobId,
        by: &Identity,
        force: bool,
        code: ReasonCode,
        reason: Option<&str>,
    ) {
        let msg = HubToBroker::Cancel(HubCancel {
            job: job.clone(),
            seq: 0,
            by: Box::new(by.clone()),
            force,
            code,
            reason: reason.map(str::to_string),
            client: None,
        });
        let payload = match serde_json::to_string(&msg) {
            Ok(p) => p,
            Err(e) => {
                self.s.diag(
                    Level::Error,
                    "cancel not serialisable",
                    &[("job", &job.0), ("error", &e.to_string())],
                );
                return;
            }
        };
        let now = self.now();
        if let Err(e) = self
            .s
            .store
            .push_outbox(job, OutboxKind::Cancel, payload, now)
            .await
        {
            self.store_failed("storage failed: cancel not stored", job, &e);
            return;
        }
        let broker = self.tracks.get(job).and_then(|t| t.order.broker.clone());
        if let Some(name) = broker {
            self.flush(&name).await;
        }
    }

    /// Ends orders that waited longer than `ttl_hours` without `accepted`.
    pub(crate) async fn expire(&mut self) {
        let o = &self.policy.outbox;
        if o.on_expire == OnExpire::Keep {
            return;
        }
        let waiting = self.tracks.values().any(|t| !t.stage.accepted());
        if !waiting {
            return;
        }
        let ttl_h = o.ttl_hours;
        let ttl = ttl_h.saturating_mul(3600);
        let pending = match self.s.store.pending_outbox().await {
            Ok(p) => p,
            Err(e) => {
                self.s.diag(
                    Level::Error,
                    "outbox unreadable",
                    &[("error", &e.to_string())],
                );
                return;
            }
        };
        let now = self.now();
        let due: Vec<JobId> = pending
            .into_iter()
            .filter(|e| e.kind == OutboxKind::Order && e.created_at.plus_secs(ttl) <= now)
            .filter(|e| self.tracks.get(&e.job).is_some_and(|t| !t.stage.accepted()))
            .map(|e| e.job)
            .collect();
        for job in due {
            self.s.diag(
                Level::Warn,
                "order not accepted within ttl_hours: dropped",
                &[("job", &job.0)],
            );
            let code = ReasonCode::Rejected(RejectReason::OutboxExpired);
            let detail = format!("ни один брокер не принял заказ за {ttl_h} ч");
            self.final_reject(&job, code, detail).await;
        }
    }
}
