use crate::diag::Diag;
use lr_abstractions::{
    Clock, Frame, Level, OutboxEntry, OutboxKind, RunnerStore, StoreError, Transport,
    TransportError,
};
use lr_core::JobId;
use lr_proto::RunnerToBroker;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

const STORE_RETRY: Duration = Duration::from_secs(1);

fn kind_of(msg: &RunnerToBroker) -> Option<OutboxKind> {
    match msg {
        RunnerToBroker::Started(_) => Some(OutboxKind::Started),
        RunnerToBroker::Chunk(_) => Some(OutboxKind::Chunk),
        RunnerToBroker::StepResult(_) => Some(OutboxKind::StepResult),
        RunnerToBroker::JobResult(_) => Some(OutboxKind::JobResult),
        _ => None,
    }
}

pub(crate) fn with_seq(payload: &str, seq: u64) -> Option<Frame> {
    let mut v: Value = serde_json::from_str(payload).ok()?;
    v.as_object_mut()?.insert("seq".into(), seq.into());
    Frame::json(&v).ok()
}

pub(crate) struct Outbox {
    pub(crate) store: Arc<dyn RunnerStore>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) diag: Arc<Diag>,
    pub(crate) wake: Arc<Notify>,
}

impl Outbox {
    pub(crate) async fn push(&self, job: &JobId, msg: &RunnerToBroker) -> Result<u64, StoreError> {
        let kind = kind_of(msg).ok_or_else(|| StoreError::Conflict("not a job message".into()))?;
        let payload = serde_json::to_string(msg).map_err(|e| StoreError::Io(e.to_string()))?;
        let seq = self
            .store
            .push_outbox(job, kind, payload, self.clock.now())
            .await?;
        self.wake.notify_one();
        Ok(seq)
    }

    pub(crate) async fn push_or_log(&self, job: &JobId, msg: &RunnerToBroker) {
        if let Err(e) = self.push(job, msg).await {
            self.diag.event(
                Level::Error,
                "job message not stored, dropped",
                &[("job", &job.0), ("error", &e.to_string())],
            );
        }
    }

    pub(crate) async fn push_until_stored(&self, job: &JobId, msg: &RunnerToBroker) -> u64 {
        let mut logged = false;
        loop {
            match self.push(job, msg).await {
                Ok(seq) => return seq,
                Err(e) => {
                    if !logged {
                        self.diag.event(
                            Level::Error,
                            "job message not stored, retrying every second",
                            &[("job", &job.0), ("error", &e.to_string())],
                        );
                        logged = true;
                    }
                    self.clock.sleep(STORE_RETRY).await;
                }
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct Replay {
    sent: HashMap<JobId, u64>,
}

impl Replay {
    pub(crate) async fn flush(
        &mut self,
        store: &dyn RunnerStore,
        transport: &dyn Transport,
        diag: &Diag,
    ) -> Result<(), TransportError> {
        let pending: Vec<OutboxEntry> = match store.pending_outbox().await {
            Ok(p) => p,
            Err(e) => {
                diag.event(
                    Level::Error,
                    "outbox unreadable",
                    &[("error", &e.to_string())],
                );
                return Ok(());
            }
        };
        for e in pending {
            let sent = self.sent.get(&e.job).copied().unwrap_or(0);
            if e.seq <= sent {
                continue;
            }
            match with_seq(&e.payload, e.seq) {
                Some(frame) => transport.send(frame).await?,
                None => diag.event(
                    Level::Error,
                    "stored job message is not valid, skipped",
                    &[("job", &e.job.0), ("seq", &e.seq.to_string())],
                ),
            }
            self.sent.insert(e.job.clone(), e.seq);
        }
        Ok(())
    }
}
