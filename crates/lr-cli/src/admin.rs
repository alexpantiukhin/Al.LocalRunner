//! The broker's administrative channel (spec 3.9): `policy reload`, `broker add`,
//! `runner remove` and the rest exist on local IPC only. A connection that did not come over
//! local IPC from a kernel-identified OS user is refused before any command is read.

use async_trait::async_trait;
use lr_abstractions::{Frame, Listener, Peer, PeerAccess, Transport};
use lr_core::Endpoint;
use lr_proto::{
    decode, receive, AdminReply, AdminRequest, IpcChannel, IpcOpen, IpcOpenReply, PeerKind,
    ProtocolError, ProtocolErrorCode, Received, PROTOCOL_VERSION,
};
use std::sync::Arc;
use std::time::Duration;

const OPEN_WITHIN: Duration = Duration::from_secs(30);

/// What the admin channel does with a command of a local peer.
#[async_trait]
pub trait AdminApi: Send + Sync {
    /// Carries out `req` for `peer`.
    async fn handle(&self, peer: &Peer, req: AdminRequest) -> AdminReply;
}

/// Serves the admin channel on `listener` until it closes.
pub async fn serve<L, A>(listener: Arc<L>, api: Arc<A>)
where
    L: Listener + ?Sized + 'static,
    A: AdminApi + ?Sized + 'static,
{
    while let Ok(t) = listener.accept().await {
        let api = api.clone();
        tokio::spawn(async move { connection(Arc::from(t), api).await });
    }
}

async fn send<M: serde::Serialize>(t: &dyn Transport, msg: &M) -> bool {
    match Frame::json(msg) {
        Ok(f) => t.send(f).await.is_ok(),
        Err(_) => false,
    }
}

async fn refuse(t: &dyn Transport, code: ProtocolErrorCode, message: &str) {
    let error = ProtocolError {
        code,
        message: message.to_string(),
        supported: (code == ProtocolErrorCode::UnsupportedVersion).then_some(PROTOCOL_VERSION),
    };
    send(t, &IpcOpenReply::Refused(error)).await;
    t.close().await;
}

fn is_local(peer: &Peer) -> bool {
    matches!(peer.endpoint, Endpoint::Ipc { .. }) && peer.access == PeerAccess::Local
}

/// Opens a local-only channel on `t`: refuses a peer that is not local IPC, then expects the
/// `hello` for `channel` and answers `ready`.
pub(crate) async fn open(t: &dyn Transport, channel: IpcChannel) -> bool {
    if !is_local(t.peer()) {
        let text = "this channel is accepted on local IPC only";
        refuse(t, ProtocolErrorCode::AdminOverNetwork, text).await;
        return false;
    }
    let first = match tokio::time::timeout(OPEN_WITHIN, t.recv()).await {
        Ok(Ok(f)) => f,
        _ => {
            t.close().await;
            return false;
        }
    };
    match decode::<IpcOpen>(first.as_bytes()) {
        Ok(IpcOpen::Hello { channel: asked, .. }) if asked != channel => {
            let text = format!("this is the {channel:?} channel");
            refuse(t, ProtocolErrorCode::Malformed, &text).await;
            false
        }
        Ok(IpcOpen::Hello { version, .. }) if version.major != PROTOCOL_VERSION.major => {
            let text = format!(
                "protocol {}.{} is not supported",
                version.major, version.minor
            );
            refuse(t, ProtocolErrorCode::UnsupportedVersion, &text).await;
            false
        }
        Ok(IpcOpen::Hello { .. }) => {
            send(
                t,
                &IpcOpenReply::Ready {
                    version: PROTOCOL_VERSION,
                },
            )
            .await
        }
        Err(e) => {
            refuse(t, ProtocolErrorCode::Malformed, &e.to_string()).await;
            false
        }
    }
}

async fn connection<A: AdminApi + ?Sized>(t: Arc<dyn Transport>, api: Arc<A>) {
    if !open(t.as_ref(), IpcChannel::Admin).await {
        return;
    }
    let peer = t.peer().clone();
    while let Ok(frame) = t.recv().await {
        let reply = match receive::<AdminRequest>(frame.as_bytes(), PeerKind::Ipc) {
            Ok(Received::Message(req)) => api.handle(&peer, req).await,
            Ok(Received::Ignored(kind)) => AdminReply::Failed {
                message: format!("unknown administrative command `{kind}`"),
            },
            Err(e) => AdminReply::Failed {
                message: e.to_string(),
            },
        };
        if !send(t.as_ref(), &reply).await {
            break;
        }
    }
    t.close().await;
}
