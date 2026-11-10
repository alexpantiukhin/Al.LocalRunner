use crate::client::send_within;
use crate::engine::{refusal, CliEvent, ConnId, Event, Shared};
use lr_abstractions::{Frame, Level, Listener, PeerAccess, Transport};
use lr_core::Endpoint;
use lr_proto::{
    decode, receive, CliReply, CliRequest, IpcChannel, IpcOpen, IpcOpenReply, PeerKind,
    ProtocolErrorCode, Received, PROTOCOL_VERSION,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

const REPLY_QUEUE: usize = 256;
const OPEN_WITHIN: Duration = Duration::from_secs(30);

pub(crate) async fn serve(
    ipc: Arc<dyn Listener>,
    shared: Arc<Shared>,
    events: mpsc::Sender<Event>,
) {
    let mut next: ConnId = 0;
    loop {
        let t = match ipc.accept().await {
            Ok(t) => t,
            Err(e) => {
                shared.diag(
                    Level::Error,
                    "hub IPC listener closed",
                    &[("error", &e.to_string())],
                );
                let _ = events.send(Event::Shutdown).await;
                return;
            }
        };
        next = next.wrapping_add(1);
        let t: Arc<dyn Transport> = Arc::from(t);
        let (shared, events) = (shared.clone(), events.clone());
        tokio::spawn(async move { connection(next, t, shared, events).await });
    }
}

async fn answer(shared: &Shared, t: &dyn Transport, reply: &IpcOpenReply) -> bool {
    match Frame::json(reply) {
        Ok(f) => send_within(shared, t, f).await,
        Err(_) => false,
    }
}

async fn refuse(shared: &Shared, t: &dyn Transport, code: ProtocolErrorCode, text: &str) {
    let mut e = refusal(code, text);
    if code == ProtocolErrorCode::UnsupportedVersion {
        e.supported = Some(PROTOCOL_VERSION);
    }
    answer(shared, t, &IpcOpenReply::Refused(e)).await;
    t.close().await;
}

async fn open(shared: &Shared, t: &dyn Transport) -> Option<String> {
    let os_user = match (&t.peer().endpoint, t.peer().access) {
        (Endpoint::Ipc { os_user }, PeerAccess::Local) => os_user.clone(),
        _ => {
            let text = "the CLI channel of the hub is local IPC only";
            refuse(shared, t, ProtocolErrorCode::Unauthorized, text).await;
            return None;
        }
    };
    let first = tokio::select! {
        r = t.recv() => r.ok(),
        _ = shared.clock.sleep(OPEN_WITHIN) => None,
    };
    let Some(first) = first else {
        t.close().await;
        return None;
    };
    match decode::<IpcOpen>(first.as_bytes()) {
        Ok(IpcOpen::Hello { channel, .. }) if channel != IpcChannel::Cli => {
            let text = "this is the CLI channel of the hub";
            refuse(shared, t, ProtocolErrorCode::Malformed, text).await;
            None
        }
        Ok(IpcOpen::Hello { version, .. }) if version.major != PROTOCOL_VERSION.major => {
            let text = format!(
                "protocol {}.{} is not supported",
                version.major, version.minor
            );
            refuse(shared, t, ProtocolErrorCode::UnsupportedVersion, &text).await;
            None
        }
        Ok(IpcOpen::Hello { .. }) => {
            let ready = IpcOpenReply::Ready {
                version: PROTOCOL_VERSION,
            };
            answer(shared, t, &ready).await.then_some(os_user)
        }
        Err(e) => {
            refuse(shared, t, ProtocolErrorCode::Malformed, &e.to_string()).await;
            None
        }
    }
}

async fn connection(
    conn: ConnId,
    t: Arc<dyn Transport>,
    shared: Arc<Shared>,
    events: mpsc::Sender<Event>,
) {
    let Some(os_user) = open(&shared, t.as_ref()).await else {
        return;
    };
    let (tx, mut rx) = mpsc::channel::<CliReply>(REPLY_QUEUE);
    let (wt, ws) = (t.clone(), shared.clone());
    tokio::spawn(async move {
        while let Some(reply) = rx.recv().await {
            let Ok(frame) = Frame::json(&reply) else {
                continue;
            };
            if !send_within(&ws, wt.as_ref(), frame).await {
                break;
            }
        }
        wt.close().await;
    });
    while let Ok(frame) = t.recv().await {
        let req = match receive::<CliRequest>(frame.as_bytes(), PeerKind::Ipc) {
            Ok(Received::Message(r)) => r,
            Ok(Received::Ignored(_)) => continue,
            Err(e) => {
                let err = e
                    .refusal()
                    .unwrap_or_else(|| refusal(ProtocolErrorCode::Malformed, e.to_string()));
                let _ = tx.try_send(CliReply::Error(err));
                continue;
            }
        };
        let event = CliEvent {
            conn,
            os_user: os_user.clone(),
            req,
            reply: tx.clone(),
        };
        if events.send(Event::Cli(Box::new(event))).await.is_err() {
            break;
        }
    }
    drop(tx);
    let _ = events.send(Event::CliClosed(conn)).await;
}
