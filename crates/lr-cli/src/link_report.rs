//! What `up` and `doctor` say about a hub or runner that is alive but has no session with its
//! broker: the process answers locally, the link does not exist.

use crate::commands::timefmt::format_time;
use crate::doctor::{Check, Status};
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_core::Timestamp;
use lr_proto::{ControlReply, IpcChannel, RunnerControl};

/// `{who}: alive but not connected to the broker since {time}` (without the time when it is
/// not known).
pub fn not_connected_line(who: &str, since: Option<Timestamp>) -> String {
    match since {
        Some(t) => format!(
            "{who}: alive but not connected to the broker since {}",
            format_time(t)
        ),
        None => format!("{who}: alive but not connected to the broker"),
    }
}

/// The line for a runner that answered `reply` to a status request: `Some` only for a runner
/// that reports it is not connected. Any other answer, as an old build gives, is no
/// information.
pub fn runner_link_line(name: &str, reply: &ControlReply) -> Option<String> {
    match reply {
        ControlReply::Link {
            connected: false,
            since,
        } => Some(not_connected_line(&format!("runner {name}"), *since)),
        _ => None,
    }
}

/// A line per broker the running hub is not connected to; empty when the hub does not answer.
pub async fn hub_lines(layout: &Layout) -> Vec<String> {
    let Ok(principal) = lr_net::current_os_principal() else {
        return Vec::new();
    };
    let Ok(c) = IpcClient::connect(&layout.hub_ipc(&principal), IpcChannel::Cli).await else {
        return Vec::new();
    };
    let brokers = crate::commands::view::known_brokers(&c).await;
    c.close().await;
    brokers
        .unwrap_or_default()
        .into_iter()
        .filter(|b| !b.available)
        .map(|b| not_connected_line(&format!("hub (broker {})", b.name), b.since))
        .collect()
}

/// The line for runner `name` when it answers that it is not connected to its broker.
pub async fn runner_line(layout: &Layout, name: &str) -> Option<String> {
    let principal = lr_net::current_os_principal().ok()?;
    let c = IpcClient::connect(
        &layout.runner_ipc(&principal, name),
        IpcChannel::RunnerControl,
    )
    .await
    .ok()?;
    let reply = c.call::<_, ControlReply>(&RunnerControl::Status).await;
    c.close().await;
    runner_link_line(name, &reply.ok()?)
}

/// Warnings for a hub or runner that is alive but not connected; nothing for a process that
/// does not answer.
pub async fn checks(layout: &Layout) -> Vec<Check> {
    let mut out = Vec::new();
    let hub = hub_lines(layout).await;
    if !hub.is_empty() {
        out.push(Check {
            name: "hub link".into(),
            status: Status::Warn,
            detail: hub.join("; "),
        });
    }
    for name in layout.runner_names() {
        if let Some(line) = runner_line(layout, &name).await {
            out.push(Check {
                name: format!("runner {name} link"),
                status: Status::Warn,
                detail: line,
            });
        }
    }
    out
}
