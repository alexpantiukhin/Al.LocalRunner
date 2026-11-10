//! The framing shared by TLS and IPC: a big-endian `u32` length, then that many bytes of one
//! JSON document. The length is checked against the limit before any byte of the body is read
//! or allocated, and the body buffer grows only as bytes arrive.
//!
//! A graceful close writes [`END_OF_STREAM`] in place of a length. Named pipes on Windows have
//! no half-close, so the end of the stream must be said in-band; the value is far above the
//! frame limit and can never be a real length.

use lr_proto::{parse_prefix, FrameError, FRAME_LIMIT, PREFIX_LEN};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The prefix that ends a stream gracefully: no frame follows it.
pub const END_OF_STREAM: u32 = u32::MAX;

fn io_error(e: io::Error) -> FrameError {
    match e.kind() {
        io::ErrorKind::UnexpectedEof => FrameError::Truncated,
        kind => FrameError::Io(kind),
    }
}

fn is_gone(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}

/// Reads one frame of at most [`FRAME_LIMIT`] bytes. [`FrameError::Closed`] is a clean end
/// between frames, [`FrameError::Truncated`] an end inside one.
pub async fn read_frame<R: AsyncRead + Unpin + ?Sized>(r: &mut R) -> Result<Vec<u8>, FrameError> {
    read_frame_limited(r, FRAME_LIMIT).await
}

/// Like [`read_frame`] with a lower `limit`, for messages read before the peer is
/// authenticated.
pub async fn read_frame_limited<R: AsyncRead + Unpin + ?Sized>(
    r: &mut R,
    limit: usize,
) -> Result<Vec<u8>, FrameError> {
    let mut prefix = [0u8; PREFIX_LEN];
    let mut got = 0;
    while got < PREFIX_LEN {
        match r.read(&mut prefix[got..]).await {
            Ok(0) if got == 0 => return Err(FrameError::Closed),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if got == 0 && is_gone(e.kind()) => return Err(FrameError::Closed),
            Err(e) => return Err(io_error(e)),
        }
    }
    if u32::from_be_bytes(prefix) == END_OF_STREAM {
        return Err(FrameError::Closed);
    }
    let len = parse_prefix(prefix)?;
    if len > limit {
        return Err(FrameError::TooLarge { len: len as u64 });
    }
    let mut body = Vec::new();
    r.take(len as u64)
        .read_to_end(&mut body)
        .await
        .map_err(io_error)?;
    if body.len() != len {
        return Err(FrameError::Truncated);
    }
    Ok(body)
}

/// Writes one frame and flushes; a payload over [`FRAME_LIMIT`] is refused before any byte is
/// written, so the stream stays usable.
pub async fn write_frame<W: AsyncWrite + Unpin + ?Sized>(
    w: &mut W,
    payload: &[u8],
) -> Result<(), FrameError> {
    let frame = lr_proto::encode_frame(payload)?;
    w.write_all(&frame).await.map_err(io_error)?;
    w.flush().await.map_err(io_error)
}

/// Writes [`END_OF_STREAM`] and flushes: the reader's next [`read_frame`] is `Closed`.
pub async fn write_end<W: AsyncWrite + Unpin + ?Sized>(w: &mut W) -> Result<(), FrameError> {
    w.write_all(&END_OF_STREAM.to_be_bytes())
        .await
        .map_err(io_error)?;
    w.flush().await.map_err(io_error)
}
