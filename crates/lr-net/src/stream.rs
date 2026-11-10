use crate::codec::{read_frame, write_end, write_frame};
use async_trait::async_trait;
use lr_abstractions::{Frame, Peer, Transport, TransportError, MAX_FRAME_BYTES};
use lr_proto::{AuthFromBroker, FrameError, ProtocolError, ProtocolErrorCode};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::Mutex;

type Reader = Box<dyn AsyncRead + Send + Unpin>;
type Writer = Box<dyn AsyncWrite + Send + Unpin>;

/// A framed connection over any byte stream: TLS after the key proof, or local IPC after the
/// kernel reported the peer's OS user.
pub struct StreamTransport {
    peer: Peer,
    reader: Mutex<Reader>,
    writer: Mutex<Writer>,
    closed: AtomicBool,
    join_only: AtomicBool,
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
            writer: Mutex::new(Box::new(w)),
            closed: AtomicBool::new(false),
            join_only: AtomicBool::new(false),
        }
    }

    pub(crate) fn join_only(self) -> StreamTransport {
        self.join_only.store(true, Ordering::SeqCst);
        self
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
            self.closed.store(true, Ordering::SeqCst);
        }
        result
    }

    async fn recv(&self) -> Result<Frame, TransportError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(TransportError::Closed);
        }
        let read = {
            let mut r = self.reader.lock().await;
            read_frame(&mut *r).await
        };
        let bytes = match read {
            Ok(bytes) => bytes,
            Err(e) => {
                self.closed.store(true, Ordering::SeqCst);
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
        if self.join_only.swap(false, Ordering::SeqCst)
            && frame_type(&bytes).as_deref() != Some("join")
        {
            return Err(self.refuse_not_join().await);
        }
        Ok(Frame::from_bytes(bytes))
    }

    async fn close(&self) {
        let was_closed = self.closed.swap(true, Ordering::SeqCst);
        let mut w = self.writer.lock().await;
        if !was_closed {
            let _ = write_end(&mut *w).await;
        }
        let _ = w.shutdown().await;
    }
}
