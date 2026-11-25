//! The local channel of a running dashboard (spec 9.1): a named pipe on Windows, a Unix socket
//! elsewhere, owner only, never TCP. A repeated `dashboard` finds the running one through it and
//! asks for a link with a new one-time token; `down` asks it to stop. The tokens travel on this
//! channel and to the terminal only, never to a log.

use crate::admin::open;
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_abstractions::{Frame, Listener, Transport};
use lr_dashboard::LinkIssuer;
use lr_net::{IpcAccess, IpcListener};
use lr_proto::{receive, DashboardReply, DashboardRequest, IpcChannel, PeerKind, Received};
use std::sync::Arc;
use tokio::sync::Notify;

/// The channel of the dashboard of this user, if one answers on it. A socket or pipe left by a
/// process that is gone does not answer, so it does not count.
pub async fn find_running(layout: &Layout) -> Option<IpcClient> {
    let principal = lr_net::current_os_principal().ok()?;
    IpcClient::connect(&layout.dashboard_ipc(&principal), IpcChannel::Dashboard)
        .await
        .ok()
}

/// Starts listening on the dashboard channel of this user; fails while another dashboard holds
/// it.
pub async fn listen(layout: &Layout) -> Result<IpcListener, String> {
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    layout.prepare_own_ipc().map_err(|e| e.to_string())?;
    IpcListener::bind(&layout.dashboard_ipc(&principal), IpcAccess::OwnerOnly)
        .await
        .map_err(|e| {
            format!(
                "the dashboard channel is taken ({e}); a dashboard of this user runs: `localrunner dashboard` prints its link"
            )
        })
}

/// Asks the dashboard on `client` for a link with a new one-time token.
pub async fn request_link(client: &IpcClient) -> Result<String, String> {
    match client
        .call::<_, DashboardReply>(&DashboardRequest::Link)
        .await?
    {
        DashboardReply::Link { url } => Ok(url),
        DashboardReply::Failed { message } => Err(message),
        DashboardReply::Stopping => Err("the dashboard is stopping".into()),
    }
}

/// Asks the running dashboard of this user to stop; `false` when none answers.
pub async fn stop_running(layout: &Layout) -> bool {
    let Some(client) = find_running(layout).await else {
        return false;
    };
    let stopping = matches!(
        client
            .call::<_, DashboardReply>(&DashboardRequest::Stop)
            .await,
        Ok(DashboardReply::Stopping)
    );
    client.close().await;
    stopping
}

/// Answers the requests of `listener` until one asks to stop (then `stop` is notified) or the
/// listener closes.
pub async fn serve(listener: Arc<IpcListener>, issuer: LinkIssuer, stop: Arc<Notify>) {
    while let Ok(t) = listener.accept().await {
        let (issuer, stop) = (issuer.clone(), stop.clone());
        tokio::spawn(async move { connection(Arc::from(t), issuer, stop).await });
    }
}

async fn send(t: &dyn Transport, reply: &DashboardReply) -> bool {
    match Frame::json(reply) {
        Ok(f) => t.send(f).await.is_ok(),
        Err(_) => false,
    }
}

/// The reply to `request`; `true` when the dashboard is to stop.
pub fn answer(request: DashboardRequest, issuer: &LinkIssuer) -> (DashboardReply, bool) {
    match request {
        DashboardRequest::Link => match issuer.fresh_url() {
            Ok(url) => (DashboardReply::Link { url }, false),
            Err(e) => (
                DashboardReply::Failed {
                    message: e.to_string(),
                },
                false,
            ),
        },
        DashboardRequest::Stop => (DashboardReply::Stopping, true),
    }
}

async fn connection(t: Arc<dyn Transport>, issuer: LinkIssuer, stop: Arc<Notify>) {
    if !open(t.as_ref(), IpcChannel::Dashboard).await {
        return;
    }
    while let Ok(frame) = t.recv().await {
        let (reply, stopping) = match receive::<DashboardRequest>(frame.as_bytes(), PeerKind::Ipc) {
            Ok(Received::Message(req)) => answer(req, &issuer),
            Ok(Received::Ignored(kind)) => (
                DashboardReply::Failed {
                    message: format!("unknown dashboard command `{kind}`"),
                },
                false,
            ),
            Err(e) => (
                DashboardReply::Failed {
                    message: e.to_string(),
                },
                false,
            ),
        };
        if !send(t.as_ref(), &reply).await {
            break;
        }
        if stopping {
            stop.notify_one();
            break;
        }
    }
    t.close().await;
}
