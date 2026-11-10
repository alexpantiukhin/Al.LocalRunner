use crate::auth::AUTH_FRAME_LIMIT;
use crate::codec::{read_frame, read_frame_limited, write_end, write_frame};
use crate::slots::Slot;
use async_trait::async_trait;
use lr_abstractions::{Frame, Peer, Transport, TransportError, MAX_FRAME_BYTES};
use lr_proto::{AuthFromBroker, FrameError, ProtocolError, ProtocolErrorCode};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{Mutex, Notify};

type Reader = Box<dyn AsyncRead + Send + Unpin>;
type Writer = Box<dyn AsyncWrite + Send + Unpin>;

const OPEN: u8 = 0;
const AWAIT_JOIN: u8 = 1;
const AWAIT_REPLY: u8 = 2;
const DONE: u8 = 3;

/// A framed connection over any byte stream: TLS after the key proof, or local IPC after the
/// kernel reported the peer's OS user.
///
/// A joining connection ([`lr_abstractions::PeerAccess::Joining`]) is one request and one
/// answer (spec 3.8): the first frame must be a `join` of at most
/// [`AUTH_FRAME_LIMIT`] bytes arriving within the join timeout, the first frame sent is the
/// last, and the transport closes the connection after it. It also shuts the connection down
/// when its lifetime ends, whatever the role does, and gives back its joining slot then or
/// when it closes, whichever comes first.
pub struct StreamTransport {
    peer: Peer,
    reader: Mutex<Reader>,
    writer: Arc<Mutex<Writer>>,
    closed: Arc<AtomicBool>,
    done: Arc<Notify>,
    phase: AtomicU8,
    join_timeout: Duration,
}

impl StreamTransport {
    pub(crate) fn new<S>(stream: S, peer: Peer) -> StreamTransport
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (r, w): (ReadHalf<S>, WriteHalf<S>) = tokio::io::split(stream);
        StreamTransport {
            peer,
            reader: Mutex::new(Box::new(r)),
            writer: Arc::new(Mutex::new(Box::new(w))),
            closed: Arc::new(AtomicBool::new(false)),
            done: Arc::new(Notify::new()),
            phase: AtomicU8::new(OPEN),
            join_timeout: Duration::ZERO,
        }
    }

    pub(crate) fn join_only(
        mut self,
        timeout: Duration,
        lifetime: Duration,
        slot: Slot,
    ) -> StreamTransport {
        self.phase = AtomicU8::new(AWAIT_JOIN);
        self.join_timeout = timeout;
        let (writer, closed, done) = (self.writer.clone(), self.closed.clone(), self.done.clone());
        tokio::spawn(async move {
            let ended = tokio::time::timeout(lifetime, done.notified()).await;
            if ended.is_err() && !closed.swap(true, Ordering::SeqCst) {
                let shut = async {
                    let mut w = writer.lock().await;
                    let _ = write_end(&mut *w).await;
                    let _ = w.shutdown().await;
                };
                let _ = tokio::time::timeout(Duration::from_secs(1), shut).await;
            }
            drop(slot);
        });
        self
    }

    fn mark_closed(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.done.notify_one();
    }

    async fn refuse_not_join(&self) -> TransportError {
        let refusal = ProtocolError {
            code: ProtocolErrorCode::Unauthorized,
            message: "unknown key: the first message must be a join with a code".into(),
            supported: None,
        };
        if let Ok(frame) = Frame::json(&AuthFromBroker::Error(refusal.clone())) {
            let _ = self.write(frame.as_bytes()).await;
        }
        self.close().await;
        crate::auth::refused(&refusal)
    }

    async fn write(&self, bytes: &[u8]) -> Result<(), TransportError> {
        let mut w = self.writer.lock().await;
        write_frame(&mut *w, bytes).await.map_err(|e| match e {
            FrameError::TooLarge { len } => TransportError::TooLarge { len: len as usize },
            FrameError::Closed | FrameError::Truncated => TransportError::Closed,
            FrameError::Io(kind) => TransportError::Io(kind.to_string()),
        })
    }
}

fn frame_type(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    value.get("type")?.as_str().map(str::to_string)
}

#[async_trait]
impl Transport for StreamTransport {
    fn peer(&self) -> &Peer {
        &self.peer
    }

    async fn send(&self, frame: Frame) -> Result<(), TransportError> {
        if frame.len() > MAX_FRAME_BYTES {
            return Err(TransportError::TooLarge { len: frame.len() });
        }
        if self.closed.load(Ordering::SeqCst) {
            return Err(TransportError::Closed);
        }
        let result = self.write(frame.as_bytes()).await;
        if matches!(result, Err(TransportError::Closed | TransportError::Io(_))) {
            self.mark_closed();
        }
        if self.phase.load(Ordering::SeqCst) != OPEN {
            self.phase.store(DONE, Ordering::SeqCst);
            self.close().await;
        }
        result
    }

    async fn recv(&self) -> Result<Frame, TransportError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(TransportError::Closed);
        }
        let (read, joining) = {
            let mut r = self.reader.lock().await;
            match self.phase.load(Ordering::SeqCst) {
                OPEN => (read_frame(&mut *r).await, false),
                AWAIT_JOIN => {
                    let limited = read_frame_limited(&mut *r, AUTH_FRAME_LIMIT);
                    match tokio::time::timeout(self.join_timeout, limited).await {
                        Ok(read) => (read, true),
                        Err(_) => {
                            drop(r);
                            self.close().await;
                            return Err(TransportError::Io("no join arrived in time".into()));
                        }
                    }
                }
                _ => return Err(TransportError::Closed),
            }
        };
        let bytes = match read {
            Ok(bytes) => bytes,
            Err(e) => {
                self.mark_closed();
                return Err(match e {
                    FrameError::Closed => TransportError::Closed,
                    FrameError::TooLarge { len } => TransportError::TooLarge { len: len as usize },
                    FrameError::Truncated => {
                        TransportError::Io("stream ended inside a frame".into())
                    }
                    FrameError::Io(kind) => TransportError::Io(kind.to_string()),
                });
            }
        };
        if joining {
            if frame_type(&bytes).as_deref() != Some("join") {
                return Err(self.refuse_not_join().await);
            }
            self.phase.store(AWAIT_REPLY, Ordering::SeqCst);
        }
        Ok(Frame::from_bytes(bytes))
    }

    async fn close(&self) {
        let was_closed = self.closed.swap(true, Ordering::SeqCst);
        self.done.notify_one();
        let mut w = self.writer.lock().await;
        if !was_closed {
            let _ = write_end(&mut *w).await;
        }
        let _ = w.shutdown().await;
    }
}

impl Drop for StreamTransport {
    fn drop(&mut self) {
        self.done.notify_one();
    }
}
