use async_trait::async_trait;
use lr_abstractions::fakes::{FakeDialer, MemoryRunnerStore};
use lr_abstractions::{
    CacheEntry, CacheKey, Dialer, Frame, OutboxBacklog, OutboxEntry, OutboxKind, Peer, RunnerStore,
    ServiceRow, StoreError, Transport, TransportError,
};
use lr_core::{JobId, Timestamp};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct Gate {
    pub hang_dials: Arc<AtomicBool>,
    pub stall_sends: Arc<AtomicBool>,
    pub sent: Arc<AtomicUsize>,
    pub refusal: Arc<Mutex<Option<String>>>,
    pub split_frames: Arc<AtomicBool>,
}

impl Gate {
    pub fn hang_dials(&self, on: bool) {
        self.hang_dials.store(on, Ordering::SeqCst);
    }

    pub fn stall_sends(&self, on: bool) {
        self.stall_sends.store(on, Ordering::SeqCst);
    }

    pub fn refuse_dials(&self, refusal: Option<&str>) {
        *self.refusal.lock().unwrap() = refusal.map(str::to_string);
    }

    pub fn split_frames(&self, on: bool) {
        self.split_frames.store(on, Ordering::SeqCst);
    }

    pub fn sent(&self) -> usize {
        self.sent.load(Ordering::SeqCst)
    }
}

pub struct GatedDialer {
    pub inner: FakeDialer,
    pub gate: Gate,
}

struct GatedTransport {
    inner: Box<dyn Transport>,
    gate: Gate,
}

#[async_trait]
impl Transport for GatedTransport {
    fn peer(&self) -> &Peer {
        self.inner.peer()
    }

    async fn send(&self, frame: Frame) -> Result<(), TransportError> {
        if self.gate.stall_sends.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        self.gate.sent.fetch_add(1, Ordering::SeqCst);
        self.inner.send(frame).await
    }

    async fn recv(&self) -> Result<Frame, TransportError> {
        let frame = self.inner.recv().await?;
        while self.gate.split_frames.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        Ok(frame)
    }

    async fn close(&self) {
        self.inner.close().await
    }
}

#[async_trait]
impl Dialer for GatedDialer {
    async fn dial(
        &self,
        address: &str,
        fingerprint: Option<&str>,
    ) -> Result<Box<dyn Transport>, TransportError> {
        if self.gate.hang_dials.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if let Some(text) = self.gate.refusal.lock().unwrap().clone() {
            return Err(TransportError::Refused(text));
        }
        let inner = self.inner.dial(address, fingerprint).await?;
        Ok(Box::new(GatedTransport {
            inner,
            gate: self.gate.clone(),
        }))
    }
}

pub struct CountingStore {
    pub inner: Arc<MemoryRunnerStore>,
    pub reads: Arc<AtomicUsize>,
    pub fail_push: Arc<AtomicBool>,
}

#[async_trait]
impl RunnerStore for CountingStore {
    async fn push_outbox(
        &self,
        job: &JobId,
        kind: OutboxKind,
        payload: String,
        at: Timestamp,
    ) -> Result<u64, StoreError> {
        if self.fail_push.load(Ordering::SeqCst) {
            return Err(StoreError::Io("disk full".into()));
        }
        self.inner.push_outbox(job, kind, payload, at).await
    }
    async fn pending_outbox(&self) -> Result<Vec<OutboxEntry>, StoreError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
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
