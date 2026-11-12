use crate::common::{ProtocolError, ProtocolErrorCode};
use lr_core::MAX_FRAME_BYTES;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt;
use std::io::{self, Read, Write};

/// Largest frame, both ways: 4 MiB (spec 3.9).
pub const FRAME_LIMIT: usize = MAX_FRAME_BYTES;

/// Bytes of the length prefix: a big-endian `u32`.
pub const PREFIX_LEN: usize = 4;

/// Why a frame could not be written or read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// The frame is larger than [`FRAME_LIMIT`].
    TooLarge {
        /// Declared or actual size.
        len: u64,
    },
    /// The stream ended cleanly between frames.
    Closed,
    /// The stream ended inside a frame.
    Truncated,
    /// Any other I/O failure.
    Io(io::ErrorKind),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::TooLarge { len } => {
                write!(f, "frame of {len} bytes exceeds {FRAME_LIMIT}")
            }
            FrameError::Closed => f.write_str("connection closed"),
            FrameError::Truncated => f.write_str("stream ended inside a frame"),
            FrameError::Io(kind) => write!(f, "i/o: {kind}"),
        }
    }
}

impl std::error::Error for FrameError {}

fn io_error(e: io::Error) -> FrameError {
    match e.kind() {
        io::ErrorKind::UnexpectedEof => FrameError::Truncated,
        kind => FrameError::Io(kind),
    }
}

/// Decodes a length prefix, refusing sizes above [`FRAME_LIMIT`] before anything is allocated.
pub fn parse_prefix(prefix: [u8; PREFIX_LEN]) -> Result<usize, FrameError> {
    let len = u32::from_be_bytes(prefix) as usize;
    if len > FRAME_LIMIT {
        return Err(FrameError::TooLarge { len: len as u64 });
    }
    Ok(len)
}

/// The prefix followed by `payload`, refused when the payload exceeds [`FRAME_LIMIT`].
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.len() > FRAME_LIMIT {
        return Err(FrameError::TooLarge {
            len: payload.len() as u64,
        });
    }
    let mut out = Vec::with_capacity(PREFIX_LEN + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Writes one frame; a payload over [`FRAME_LIMIT`] is refused before any byte is written.
pub fn write_frame<W: Write>(w: &mut W, payload: &[u8]) -> Result<(), FrameError> {
    let frame = encode_frame(payload)?;
    w.write_all(&frame).map_err(io_error)?;
    w.flush().map_err(io_error)
}

/// Reads one frame. The length prefix is checked before the body is read or allocated;
/// [`FrameError::Closed`] means a clean end between frames.
pub fn read_frame<R: Read>(r: &mut R) -> Result<Vec<u8>, FrameError> {
    let mut prefix = [0u8; PREFIX_LEN];
    let mut got = 0;
    while got < PREFIX_LEN {
        match r.read(&mut prefix[got..]) {
            Ok(0) if got == 0 => return Err(FrameError::Closed),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io_error(e)),
        }
    }
    let len = parse_prefix(prefix)?;
    let mut body = Vec::new();
    r.take(len as u64)
        .read_to_end(&mut body)
        .map_err(io_error)?;
    if body.len() != len {
        return Err(FrameError::Truncated);
    }
    Ok(body)
}

/// Which side of the connection a document came from, for local-only messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerKind {
    /// Local IPC, the OS user checked by the kernel.
    Ipc,
    /// A network connection.
    Network,
}

/// A protocol message: a serde enum tagged by `type`.
pub trait Message: Serialize + DeserializeOwned {
    /// Every `type` tag of the enum.
    const TYPES: &'static [&'static str];

    /// Whether the channel exists only on local IPC (CLI to hub, admin commands, runner
    /// control); such a document from a network peer is refused.
    const LOCAL_ONLY: bool;

    /// Checks invariants serde cannot express; a violation makes the document malformed.
    fn check(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Names of the administrative commands; they exist on local IPC only and a network peer that
/// sends one is refused (spec 3.9).
pub const ADMIN_TYPES: &[&str] = <crate::ipc::AdminRequest as Message>::TYPES;

/// Why a document is not a message of its channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The document exceeds [`FRAME_LIMIT`].
    TooLarge {
        /// Its size.
        len: u64,
    },
    /// Not JSON, no `type`, wrong fields or a violated invariant.
    Malformed(String),
    /// A `type` this channel does not know: possibly a message of a newer minor version.
    UnknownType(String),
    /// A local-only or administrative message arrived over the network.
    NotOverNetwork(String),
}

impl DecodeError {
    /// Whether the receiver just skips the document and keeps the connection (unknown types
    /// of a newer minor version).
    pub fn is_ignorable(&self) -> bool {
        matches!(self, DecodeError::UnknownType(_))
    }

    /// The error message to send back, `None` for an ignorable document.
    pub fn refusal(&self) -> Option<ProtocolError> {
        let (code, message) = match self {
            DecodeError::UnknownType(_) => return None,
            DecodeError::TooLarge { len } => (
                ProtocolErrorCode::FrameTooLarge,
                format!("document of {len} bytes exceeds {FRAME_LIMIT}"),
            ),
            DecodeError::Malformed(m) => (ProtocolErrorCode::Malformed, m.clone()),
            DecodeError::NotOverNetwork(t) => (
                ProtocolErrorCode::AdminOverNetwork,
                format!("`{t}` is accepted on local IPC only"),
            ),
        };
        Some(ProtocolError {
            code,
            message,
            supported: None,
        })
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::TooLarge { len } => write!(f, "document of {len} bytes too large"),
            DecodeError::Malformed(m) => write!(f, "malformed message: {m}"),
            DecodeError::UnknownType(t) => write!(f, "unknown message type `{t}`"),
            DecodeError::NotOverNetwork(t) => write!(f, "`{t}` not accepted over the network"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Serializes a message as one JSON document, refused over [`FRAME_LIMIT`].
pub fn encode<T: Message>(msg: &T) -> Result<Vec<u8>, FrameError> {
    let bytes = serde_json::to_vec(msg).map_err(|_| FrameError::Io(io::ErrorKind::InvalidData))?;
    if bytes.len() > FRAME_LIMIT {
        return Err(FrameError::TooLarge {
            len: bytes.len() as u64,
        });
    }
    Ok(bytes)
}

/// Parses one JSON document as a message of channel `T`. A document over [`FRAME_LIMIT`] is
/// refused unparsed; an unknown `type` is [`DecodeError::UnknownType`] (an administrative one
/// is [`DecodeError::NotOverNetwork`]).
pub fn decode<T: Message>(bytes: &[u8]) -> Result<T, DecodeError> {
    if bytes.len() > FRAME_LIMIT {
        return Err(DecodeError::TooLarge {
            len: bytes.len() as u64,
        });
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| DecodeError::Malformed(e.to_string()))?;
    let tag = match value.get("type") {
        Some(serde_json::Value::String(t)) => t.clone(),
        Some(_) => return Err(DecodeError::Malformed("`type` is not a string".into())),
        None => return Err(DecodeError::Malformed("missing `type`".into())),
    };
    if !T::TYPES.contains(&tag.as_str()) {
        if !T::LOCAL_ONLY && ADMIN_TYPES.contains(&tag.as_str()) {
            return Err(DecodeError::NotOverNetwork(tag));
        }
        return Err(DecodeError::UnknownType(tag));
    }
    let msg: T =
        serde_json::from_value(value).map_err(|e| DecodeError::Malformed(e.to_string()))?;
    msg.check().map_err(DecodeError::Malformed)?;
    Ok(msg)
}

/// What a receiver does with a document: handle the message, or skip a type it does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received<T> {
    /// A message of the channel.
    Message(T),
    /// An unknown type, skipped; the caller logs and counts it. Skipping is safe only because
    /// senders never exceed the negotiated minor version.
    Ignored(String),
}

/// Like [`decode_for_peer`], but an unknown type is a value, not an error.
pub fn receive<T: Message>(bytes: &[u8], peer: PeerKind) -> Result<Received<T>, DecodeError> {
    match decode_for_peer(bytes, peer) {
        Ok(m) => Ok(Received::Message(m)),
        Err(DecodeError::UnknownType(t)) => Ok(Received::Ignored(t)),
        Err(e) => Err(e),
    }
}

/// Like [`decode`], and refuses a local-only channel when the peer is on the network.
pub fn decode_for_peer<T: Message>(bytes: &[u8], peer: PeerKind) -> Result<T, DecodeError> {
    if bytes.len() > FRAME_LIMIT {
        return Err(DecodeError::TooLarge {
            len: bytes.len() as u64,
        });
    }
    if T::LOCAL_ONLY && peer == PeerKind::Network {
        let tag = serde_json::from_slice::<serde_json::Value>(bytes)
            .ok()
            .and_then(|v| v.get("type").and_then(|t| t.as_str().map(str::to_string)))
            .unwrap_or_default();
        return Err(DecodeError::NotOverNetwork(tag));
    }
    decode(bytes)
}

macro_rules! message {
    ($ty:ty, local: $local:expr, [$($tag:literal),* $(,)?]) => {
        impl $crate::frame::Message for $ty {
            const TYPES: &'static [&'static str] = &[$($tag),*];
            const LOCAL_ONLY: bool = $local;
        }
    };
    ($ty:ty, local: $local:expr, [$($tag:literal),* $(,)?], check: $check:expr) => {
        impl $crate::frame::Message for $ty {
            const TYPES: &'static [&'static str] = &[$($tag),*];
            const LOCAL_ONLY: bool = $local;
            fn check(&self) -> Result<(), String> {
                ($check)(self)
            }
        }
    };
}
pub(crate) use message;
