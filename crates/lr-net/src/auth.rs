//! The key proof that opens every TLS connection (spec 3.9, 13.2 "подпись nonce"), before any
//! protocol message:
//!
//! 1. broker → peer `challenge` with 32 fresh random bytes, new for every connection;
//! 2. peer → broker `proof`: its public key and the signature of
//!    [`crate::keys::challenge_message`] (context, broker fingerprint, TLS channel binding,
//!    nonce);
//! 3. broker → peer `authenticated`, or `error` and close: a bad signature is `unauthorized`,
//!    a revoked key `key_revoked` (decided by the [`KeyGate`]).
//!
//! Frames of this exchange, and the single `join` of a joining peer, are capped at
//! [`AUTH_FRAME_LIMIT`], so an unauthenticated peer cannot make the broker buffer more.

use crate::codec::{read_frame_limited, write_frame};
use crate::fingerprint::Fingerprint;
use crate::keys::{canonical_public_key, fresh_nonce, verify_challenge, ClientKey, NONCE_LEN};
use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use lr_abstractions::TransportError;
use lr_proto::{decode, encode, AuthFromBroker, AuthToBroker, ProtocolError, ProtocolErrorCode};
use tokio::io::{AsyncRead, AsyncWrite};

/// Largest frame an unauthenticated or joining peer may send: 64 KiB.
pub const AUTH_FRAME_LIMIT: usize = 64 * 1024;

/// What the broker lets a key that proved itself do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admit {
    /// A registered, unrevoked key.
    Member,
    /// A key the broker does not know, while a join code is open: the connection may send
    /// one `join` and gets one reply; anything else is refused.
    Joining,
    /// Refused, with the reason sent to the peer.
    Refused(ProtocolError),
}

/// Decides, after the signature holds, whether a key may go on: the machine registry of the
/// broker (revoked keys are refused at the handshake, spec 16.4).
#[async_trait]
pub trait KeyGate: Send + Sync {
    /// The verdict for `public_key`.
    async fn admit(&self, public_key: &str) -> Admit;
}

/// The error a refusal becomes on the dialing side: `Refused("<code>: <message>")`, the code
/// in its wire spelling so [`refusal_code`] can read it back.
pub fn refused(error: &ProtocolError) -> TransportError {
    let code = serde_json::to_value(error.code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "other".into());
    TransportError::Refused(format!("{code}: {}", error.message))
}

/// The protocol code of a refusal made by [`refused`]; a runner kills its jobs at once on
/// `unauthorized` and `key_revoked` (spec 3.9).
pub fn refusal_code(error: &TransportError) -> Option<ProtocolErrorCode> {
    let TransportError::Refused(text) = error else {
        return None;
    };
    let code = text.split(':').next()?;
    serde_json::from_value(serde_json::Value::String(code.to_string())).ok()
}

fn unauthorized(message: &str) -> ProtocolError {
    ProtocolError {
        code: ProtocolErrorCode::Unauthorized,
        message: message.to_string(),
        supported: None,
    }
}

fn io(e: impl std::fmt::Display) -> TransportError {
    TransportError::Io(e.to_string())
}

async fn send<W: AsyncWrite + Unpin, T: lr_proto::Message>(
    w: &mut W,
    msg: &T,
) -> Result<(), TransportError> {
    let bytes = encode(msg).map_err(io)?;
    write_frame(w, &bytes).await.map_err(io)
}

async fn recv<R: AsyncRead + Unpin, T: lr_proto::Message>(r: &mut R) -> Result<T, TransportError> {
    let bytes = read_frame_limited(r, AUTH_FRAME_LIMIT)
        .await
        .map_err(|e| match e {
            lr_proto::FrameError::Closed => TransportError::Closed,
            other => io(other),
        })?;
    decode(&bytes).map_err(io)
}

/// Broker side: challenges the peer and checks its proof. Returns the proven key and the
/// gate's verdict; a refusal has already been sent to the peer.
pub(crate) async fn accept_proof<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    own: &Fingerprint,
    binding: &[u8],
    gate: &dyn KeyGate,
) -> Result<(String, Admit), TransportError> {
    let nonce = fresh_nonce().map_err(io)?;
    send(
        stream,
        &AuthFromBroker::Challenge {
            nonce: URL_SAFE_NO_PAD.encode(nonce),
        },
    )
    .await?;
    let AuthToBroker::Proof {
        public_key,
        signature,
    } = recv(stream).await?;
    let proven = verify_challenge(&public_key, own, binding, &nonce, &signature)
        .and_then(|()| canonical_public_key(&public_key));
    let (public_key, verdict) = match proven {
        Ok(key) => {
            let verdict = gate.admit(&key).await;
            (key, verdict)
        }
        Err(e) => (public_key, Admit::Refused(unauthorized(&e.to_string()))),
    };
    match &verdict {
        Admit::Refused(error) => {
            let _ = send(stream, &AuthFromBroker::Error(error.clone())).await;
            Err(refused(error))
        }
        _ => {
            send(stream, &AuthFromBroker::Authenticated).await?;
            Ok((public_key, verdict))
        }
    }
}

/// Peer side: signs the broker's challenge with `key`, bound to the pinned fingerprint.
pub(crate) async fn prove<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    pin: &Fingerprint,
    binding: &[u8],
    key: &ClientKey,
) -> Result<(), TransportError> {
    let nonce = match recv(stream).await? {
        AuthFromBroker::Challenge { nonce } => nonce,
        AuthFromBroker::Error(error) => return Err(refused(&error)),
        AuthFromBroker::Authenticated => {
            return Err(TransportError::Refused(
                "broker skipped the challenge".into(),
            ))
        }
    };
    let nonce = URL_SAFE_NO_PAD
        .decode(&nonce)
        .ok()
        .filter(|n| n.len() == NONCE_LEN)
        .ok_or_else(|| TransportError::Refused("malformed challenge".into()))?;
    send(
        stream,
        &AuthToBroker::Proof {
            public_key: key.public_key(),
            signature: key.sign_challenge(pin, binding, &nonce),
        },
    )
    .await?;
    match recv(stream).await {
        Ok(AuthFromBroker::Authenticated) => Ok(()),
        Ok(AuthFromBroker::Error(error)) => Err(refused(&error)),
        Ok(AuthFromBroker::Challenge { .. }) => Err(TransportError::Refused(
            "broker sent a second challenge".into(),
        )),
        Err(TransportError::Closed) => Err(TransportError::Refused(
            "broker closed the connection during the key proof".into(),
        )),
        Err(e) => Err(e),
    }
}
