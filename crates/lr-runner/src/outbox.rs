use crate::diag::Diag;
use lr_abstractions::{Clock, Frame, Level, OutboxEntry, OutboxKind, RunnerStore, StoreError};
use lr_core::JobId;
use lr_proto::{Chunk, RunnerToBroker};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

const STORE_RETRY: Duration = Duration::from_secs(1);
const TAIL_LIMIT: usize = 1024;

#[derive(Default)]
pub(crate) struct Tail {
    entries: VecDeque<OutboxEntry>,
    overflow: bool,
}

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
    pub(crate) tail: Mutex<Tail>,
}

impl Outbox {
    fn remember(&self, entry: OutboxEntry) {
        let mut tail = self.tail.lock().unwrap_or_else(|p| p.into_inner());
        if tail.overflow || tail.entries.len() >= TAIL_LIMIT {
            tail.entries.clear();
            tail.overflow = true;
        } else {
            tail.entries.push_back(entry);
        }
    }

    fn drain_tail(&self) -> (Vec<OutboxEntry>, bool) {
        let mut tail = self.tail.lock().unwrap_or_else(|p| p.into_inner());
        let overflow = std::mem::take(&mut tail.overflow);
        (tail.entries.drain(..).collect(), overflow)
    }

    pub(crate) async fn push_once(&self, job: &JobId, msg: &RunnerToBroker, what: &str) {
        if let Err(e) = self.push(job, msg).await {
            self.diag.event(
                Level::Error,
                what,
                &[("job", &job.0), ("error", &e.to_string())],
            );
        }
    }

    pub(crate) async fn push(&self, job: &JobId, msg: &RunnerToBroker) -> Result<u64, StoreError> {
        let kind = kind_of(msg).ok_or_else(|| StoreError::Conflict("not a job message".into()))?;
        let payload = serde_json::to_string(msg).map_err(|e| StoreError::Io(e.to_string()))?;
        let created_at = self.clock.now();
        let seq = self
            .store
            .push_outbox(job, kind, payload.clone(), created_at)
            .await?;
        self.remember(OutboxEntry {
            job: job.clone(),
            seq,
            kind,
            payload,
            created_at,
        });
        self.wake.notify_one();
        Ok(seq)
    }

    pub(crate) async fn push_or_log(&self, job: &JobId, msg: &RunnerToBroker) {
        self.push_once(job, msg, "job message not stored, dropped")
            .await;
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

fn placeholder(e: &OutboxEntry) -> Option<Frame> {
    Frame::json(&RunnerToBroker::Chunk(Chunk {
        job: e.job.clone(),
        seq: e.seq,
        step: None,
        text: format!(
            "[localrunner: stored message {} is unreadable]
",
            e.seq
        ),
    }))
    .ok()
}

#[derive(Default)]
pub(crate) struct Replay {
    sent: HashMap<JobId, u64>,
    synced: bool,
}

impl Replay {
    pub(crate) async fn frames(&mut self, outbox: &Outbox) -> Vec<Frame> {
        let (recent, overflow) = outbox.drain_tail();
        let entries = if self.synced && !overflow {
            recent
        } else {
            match outbox.store.pending_outbox().await {
                Ok(p) => {
                    self.synced = true;
                    p
                }
                Err(e) => {
                    outbox.diag.event(
                        Level::Error,
                        "outbox unreadable",
                        &[("error", &e.to_string())],
                    );
                    self.synced = false;
                    return Vec::new();
                }
            }
        };
        let mut frames = Vec::new();
        for e in entries {
            let sent = self.sent.get(&e.job).copied().unwrap_or(0);
            if e.seq <= sent {
                continue;
            }
            let frame = with_seq(&e.payload, e.seq).or_else(|| {
                outbox.diag.event(
                    Level::Error,
                    "stored job message is not valid, a placeholder is sent",
                    &[("job", &e.job.0), ("seq", &e.seq.to_string())],
                );
                placeholder(&e)
            });
            if let Some(f) = frame {
                frames.push(f);
            }
            self.sent.insert(e.job.clone(), e.seq);
        }
        frames
    }
}
