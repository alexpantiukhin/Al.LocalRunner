use async_trait::async_trait;
use lr_core::Endpoint;
pub use lr_core::MAX_FRAME_BYTES;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt;

/// One framed message: a single JSON document. The transport moves frames whole and in order;
/// what the document means is defined by the protocol crate.
#[derive(Clone, PartialEq, Eq)]
pub struct Frame(Vec<u8>);

impl Frame {
    /// Serializes `value` as one JSON frame.
    pub fn json<T: Serialize>(value: &T) -> Result<Frame, serde_json::Error> {
        serde_json::to_vec(value).map(Frame)
    }

    /// Wraps already encoded JSON bytes.
    pub fn from_bytes(bytes: Vec<u8>) -> Frame {
        Frame(bytes)
    }

    /// Parses the frame as `T`.
    pub fn parse<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(&self.0)
    }

    /// The encoded bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Takes the encoded bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    /// Size in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the frame has no bytes.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Frame({} bytes)", self.0.len())
    }
}

/// The other side of a connection, as established by the transport before any frame is read.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Peer {
    /// How the peer is connected: local IPC with the OS user checked by the kernel, or the
    /// network source address. Zones are computed from it on every connection (spec 3.7).
    pub endpoint: Endpoint,
    /// Public key the peer proved to hold (signed nonce for clients and runners, pinned
    /// certificate fingerprint for a dialed broker). `None` on local IPC, which needs no key.
    /// A joiner proves the fresh key it is about to register, so a joining connection carries
    /// that key too and its `join` must name the same one. The transport may refuse a revoked
    /// key at the handshake; whether a key is registered is otherwise decided by the role from
    /// its store.
    pub key: Option<String>,
}

/// Why a transport operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The connection is closed; nothing more can be sent or received on it.
    Closed,
    /// The frame exceeds [`MAX_FRAME_BYTES`].
    TooLarge {
        /// Size of the refused frame.
        len: usize,
    },
    /// Nobody answers at this address.
    Unreachable(String),
    /// The other side refused the connection (wrong fingerprint, unknown or revoked key).
    Refused(String),
    /// Any other I/O failure.
    Io(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::Closed => f.write_str("connection closed"),
            TransportError::TooLarge { len } => {
                write!(f, "frame of {len} bytes exceeds {MAX_FRAME_BYTES}")
            }
            TransportError::Unreachable(a) => write!(f, "unreachable: {a}"),
            TransportError::Refused(r) => write!(f, "refused: {r}"),
            TransportError::Io(e) => write!(f, "i/o: {e}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// A persistent connection carrying frames both ways (spec 3.9). Sending and receiving may
/// run concurrently from different tasks; frames arrive whole and in send order, or the
/// connection closes. Delivery is not acknowledged here: reliability comes from the outbox,
/// `seq` and resume on top of it.
#[async_trait]
pub trait Transport: Send + Sync {
    /// The other side of the connection.
    fn peer(&self) -> &Peer;

    /// Sends one frame; [`TransportError::TooLarge`] leaves the connection usable.
    async fn send(&self, frame: Frame) -> Result<(), TransportError>;

    /// Receives the next frame; [`TransportError::Closed`] once the connection is gone.
    async fn recv(&self) -> Result<Frame, TransportError>;

    /// Closes the connection gracefully: frames sent before are still delivered, then the other
    /// side's `recv` fails with `Closed`; this end can neither send nor receive afterwards.
    async fn close(&self);
}

/// Accepts incoming connections (broker: TLS addresses of `[listen]` and the local IPC).
#[async_trait]
pub trait Listener: Send + Sync {
    /// Waits for the next connection whose transport handshake succeeded.
    async fn accept(&self) -> Result<Box<dyn Transport>, TransportError>;
}

/// Opens outgoing connections (hub and runner to a broker; runners have no listening ports).
#[async_trait]
pub trait Dialer: Send + Sync {
    /// Connects to the broker at `address` (`host:port`, or the local IPC name). With a
    /// `fingerprint` (from the join code or the hub's broker list) the connection is refused
    /// unless the broker presents exactly that certificate fingerprint, which then becomes the
    /// peer key; `None` is for local IPC.
    async fn dial(
        &self,
        address: &str,
        fingerprint: Option<&str>,
    ) -> Result<Box<dyn Transport>, TransportError>;
}
