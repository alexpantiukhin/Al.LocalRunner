use async_trait::async_trait;
use lr_abstractions::fakes::FakeListener;
use lr_abstractions::{Frame, Listener, Peer, Transport, TransportError};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct Stall {
    pub on: Arc<AtomicBool>,
    pub closed: Arc<AtomicUsize>,
}

impl Stall {
    pub fn start(&self) {
        self.on.store(true, Ordering::SeqCst);
    }

    pub fn closes(&self) -> usize {
        self.closed.load(Ordering::SeqCst)
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
            std::future::pending::<()>().await;
        }
        self.inner.send(frame).await
    }

    async fn recv(&self) -> Result<Frame, TransportError> {
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
