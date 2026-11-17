//! A runner's local control channel (spec 3.9): `down` and `logging reload` reach the runner
//! here, never over the network.

use crate::admin::open;
use crate::runner_reload::RunnerReloader;
use lr_abstractions::{Frame, Listener, Transport};
use lr_hub::ReloadLogging;
use lr_proto::{receive, ControlReply, DownMode, IpcChannel, PeerKind, Received, RunnerControl};
use lr_runner::AgentControl;
use std::sync::Arc;

/// Serves the control channel of one runner on `listener` until it closes; `reload` rereads
/// the runner's `logging.toml` and `settings` its `runner.toml`.
pub async fn serve(
    listener: Arc<dyn Listener>,
    control: AgentControl,
    reload: ReloadLogging,
    settings: Arc<RunnerReloader>,
) {
    while let Ok(t) = listener.accept().await {
        let (control, reload, settings) = (control.clone(), reload.clone(), settings.clone());
        tokio::spawn(async move { connection(Arc::from(t), control, reload, settings).await });
    }
}

async fn connection(
    t: Arc<dyn Transport>,
    control: AgentControl,
    reload: ReloadLogging,
    settings: Arc<RunnerReloader>,
) {
    if !open(t.as_ref(), IpcChannel::RunnerControl).await {
        return;
    }
    while let Ok(frame) = t.recv().await {
        let reply = match receive::<RunnerControl>(frame.as_bytes(), PeerKind::Ipc) {
            Ok(Received::Message(RunnerControl::Down { mode, reason })) => {
                match mode {
                    DownMode::Drain => {
                        control
                            .drain(reason.unwrap_or(lr_proto::DrainReason::Down))
                            .await
                    }
                    DownMode::Cancel => {
                        control
                            .cancel_and_stop(reason.unwrap_or(lr_proto::DrainReason::Down))
                            .await
                    }
                }
                ControlReply::Ok
            }
            Ok(Received::Message(RunnerControl::LoggingReload)) => match reload() {
                Ok(()) => ControlReply::Ok,
                Err(message) => ControlReply::Failed { message },
            },
            Ok(Received::Message(RunnerControl::Reload)) => {
                let settings = settings.clone();
                match tokio::task::spawn_blocking(move || settings.reload()).await {
                    Ok(Ok(restart_required)) => ControlReply::Reloaded { restart_required },
                    Ok(Err(message)) => ControlReply::Failed { message },
                    Err(e) => ControlReply::Failed {
                        message: e.to_string(),
                    },
                }
            }
            Ok(Received::Message(RunnerControl::DropOutbox { job })) => {
                match control.drop_outbox(&lr_core::JobId(job)).await {
                    Ok(closed) => ControlReply::Dropped { closed },
                    Err(message) => ControlReply::Failed { message },
                }
            }
            Ok(Received::Message(RunnerControl::Status)) => {
                let (connected, since) = control.link();
                ControlReply::Link {
                    connected,
                    since,
                    config_hash: Some(settings.config_hash()),
                }
            }
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
