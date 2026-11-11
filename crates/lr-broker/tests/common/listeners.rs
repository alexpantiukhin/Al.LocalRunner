use async_trait::async_trait;
use lr_abstractions::fakes::FakeListener;
use lr_abstractions::{Frame, Listener, Peer, Transport, TransportError};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct Stall {
    pub on: Arc<AtomicBool>,
    pub closed: Arc<AtomicUsize>,
    pub sent: Arc<AtomicUsize>,
    pub reads: Arc<AtomicUsize>,
    permits: Arc<Semaphore>,
}

impl Default for Stall {
    fn default() -> Stall {
        Stall {
            on: Arc::default(),
            closed: Arc::default(),
            sent: Arc::default(),
            reads: Arc::default(),
            permits: Arc::new(Semaphore::new(0)),
        }
    }
}

impl Stall {
    pub fn start(&self) {
        self.on.store(true, Ordering::SeqCst);
    }

    pub fn closes(&self) -> usize {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn let_through(&self, frames: usize) {
        self.permits.add_permits(frames);
    }

    pub fn sent(&self) -> usize {
        self.sent.load(Ordering::SeqCst)
    }

    pub fn reads_started(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

pub struct StallListener {
    pub inner: FakeListener,
    pub stall: Stall,
}

struct StallTransport {
    inner: Box<dyn Transport>,
    stall: Stall,
}

#[async_trait]
impl Listener for StallListener {
    async fn accept(&self) -> Result<Box<dyn Transport>, TransportError> {
        let inner = self.inner.accept().await?;
        Ok(Box::new(StallTransport {
            inner,
            stall: self.stall.clone(),
        }))
    }
}

#[async_trait]
impl Transport for StallTransport {
    fn peer(&self) -> &Peer {
        self.inner.peer()
    }

    async fn send(&self, frame: Frame) -> Result<(), TransportError> {
        if self.stall.on.load(Ordering::SeqCst) {
            match self.stall.permits.acquire().await {
                Ok(p) => p.forget(),
                Err(_) => std::future::pending::<()>().await,
            }
        }
        let sent = self.inner.send(frame).await;
        self.stall.sent.fetch_add(1, Ordering::SeqCst);
        sent
    }

    async fn recv(&self) -> Result<Frame, TransportError> {
        self.stall.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.recv().await
    }

    async fn close(&self) {
        self.stall.closed.fetch_add(1, Ordering::SeqCst);
        self.inner.close().await;
    }
}

pub struct FailingListener {
    pub calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Listener for FailingListener {
    async fn accept(&self) -> Result<Box<dyn Transport>, TransportError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(TransportError::Io("socket gone".into()))
    }
}
