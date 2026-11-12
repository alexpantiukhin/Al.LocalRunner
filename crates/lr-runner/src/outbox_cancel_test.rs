use super::{Outbox, Replay};
use crate::diag::Diag;
use async_trait::async_trait;
use lr_abstractions::fakes::{ManualClock, MemoryLogSink, MemoryRunnerStore};
use lr_abstractions::{
    CacheEntry, CacheKey, OutboxBacklog, OutboxEntry, OutboxKind, RunnerStore, ServiceRow,
    StoreError,
};
use lr_core::{JobId, Timestamp};
use lr_proto::{Chunk, RunnerToBroker};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Default)]
struct StuckAfterWrite {
    inner: MemoryRunnerStore,
    stuck: AtomicBool,
}

#[async_trait]
impl RunnerStore for StuckAfterWrite {
    async fn push_outbox(
        &self,
        job: &JobId,
        kind: OutboxKind,
        payload: String,
        at: Timestamp,
    ) -> Result<u64, StoreError> {
        let seq = self.inner.push_outbox(job, kind, payload, at).await?;
        if self.stuck.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(seq)
    }
    async fn pending_outbox(&self) -> Result<Vec<OutboxEntry>, StoreError> {
        self.inner.pending_outbox().await
    }
    async fn outbox_backlog(&self) -> Result<OutboxBacklog, StoreError> {
        self.inner.outbox_backlog().await
    }
    async fn ack_outbox(&self, job: &JobId, seq: u64) -> Result<usize, StoreError> {
        self.inner.ack_outbox(job, seq).await
    }
    async fn discard_outbox(&self, job: &JobId) -> Result<usize, StoreError> {
        self.inner.discard_outbox(job).await
    }
    async fn clear_outbox(&self) -> Result<usize, StoreError> {
        self.inner.clear_outbox().await
    }
    async fn accept_seq(&self, job: &JobId, seq: u64) -> Result<bool, StoreError> {
        self.inner.accept_seq(job, seq).await
    }
    async fn last_seq(&self, job: &JobId) -> Result<u64, StoreError> {
        self.inner.last_seq(job).await
    }
    async fn forget_finished(&self, before: Timestamp) -> Result<usize, StoreError> {
        self.inner.forget_finished(before).await
    }
    async fn upsert_service(&self, service: ServiceRow) -> Result<(), StoreError> {
        self.inner.upsert_service(service).await
    }
    async fn services(&self) -> Result<Vec<ServiceRow>, StoreError> {
        self.inner.services().await
    }
    async fn remove_service(&self, name: &str) -> Result<(), StoreError> {
        self.inner.remove_service(name).await
    }
    async fn upsert_cache(&self, entry: CacheEntry) -> Result<(), StoreError> {
        self.inner.upsert_cache(entry).await
    }
    async fn cache_entries(&self) -> Result<Vec<CacheEntry>, StoreError> {
        self.inner.cache_entries().await
    }
    async fn remove_cache(&self, key: &CacheKey) -> Result<(), StoreError> {
        self.inner.remove_cache(key).await
    }
}

fn chunk(job: &JobId, text: &str) -> RunnerToBroker {
    RunnerToBroker::Chunk(Chunk {
        job: job.clone(),
        seq: 0,
        step: None,
        text: text.into(),
    })
}

#[tokio::test]
async fn a_push_cut_off_after_its_write_is_not_lost_from_the_next_replay() {
    let store = Arc::new(StuckAfterWrite::default());
    let clock = Arc::new(ManualClock::new(Timestamp(1_000)));
    let outbox = Outbox {
        store: store.clone(),
        clock: clock.clone(),
        diag: Arc::new(Diag {
            log: Arc::new(MemoryLogSink::default()),
            clock,
        }),
        wake: Arc::new(Notify::new()),
        tail: Default::default(),
    };
    let job = JobId("j".into());
    let mut replay = Replay::default();
    assert!(replay.frames(&outbox).await.is_empty());
    store.stuck.store(true, Ordering::SeqCst);
    let cut = chunk(&job, "cut");
    tokio::select! {
        biased;
        _ = outbox.push(&job, &cut) => panic!("the write was meant to hang"),
        _ = tokio::task::yield_now() => {}
    }
    store.stuck.store(false, Ordering::SeqCst);
    outbox.push(&job, &chunk(&job, "next")).await.unwrap();
    let frames = replay.frames(&outbox).await;
    let seqs: Vec<u64> = frames
        .iter()
        .map(|f| {
            f.parse::<serde_json::Value>().unwrap()["seq"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(seqs, vec![1, 2]);
}
