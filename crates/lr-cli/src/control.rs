//! A runner's local control channel (spec 3.9): `down` and `logging reload` reach the runner
//! here, never over the network.

use crate::admin::open;
use lr_abstractions::{Frame, Listener, Transport};
use lr_hub::ReloadLogging;
use lr_proto::{receive, ControlReply, DownMode, IpcChannel, PeerKind, Received, RunnerControl};
use lr_runner::AgentControl;
use std::sync::Arc;

/// Serves the control channel of one runner on `listener` until it closes; `reload` rereads
/// the runner's `logging.toml`.
pub async fn serve(listener: Arc<dyn Listener>, control: AgentControl, reload: ReloadLogging) {
    while let Ok(t) = listener.accept().await {
        let (control, reload) = (control.clone(), reload.clone());
        tokio::spawn(async move { connection(Arc::from(t), control, reload).await });
    }
}

async fn connection(t: Arc<dyn Transport>, control: AgentControl, reload: ReloadLogging) {
    if !open(t.as_ref(), IpcChannel::RunnerControl).await {
        return;
    }
    while let Ok(frame) = t.recv().await {
        let reply = match receive::<RunnerControl>(frame.as_bytes(), PeerKind::Ipc) {
            Ok(Received::Message(RunnerControl::Down { mode })) => {
                match mode {
                    DownMode::Drain => control.drain().await,
                    DownMode::Cancel => control.cancel_and_stop().await,
                }
                ControlReply::Ok
            }
            Ok(Received::Message(RunnerControl::LoggingReload)) => match reload() {
                Ok(()) => ControlReply::Ok,
                Err(message) => ControlReply::Failed { message },
            },
            Ok(Received::Ignored(kind)) => ControlReply::Failed {
                message: format!("unknown control command `{kind}`"),
            },
            Err(e) => ControlReply::Failed {
                message: e.to_string(),
            },
        };
        let sent = match Frame::json(&reply) {
            Ok(f) => t.send(f).await.is_ok(),
            Err(_) => false,
        };
        if !sent {
            break;
        }
    }
    t.close().await;
}
