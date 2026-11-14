//! `logging reload` (spec 12.1): every running role of the installation rereads
//! `logging.toml` — runners through their control channel, the hub through its CLI channel,
//! the broker through its admin channel.

use super::down_cmd::{down_targets, Target};
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_proto::RunnerControl;
use lr_proto::{AdminReply, AdminRequest, CliReply, CliRequest, ControlReply, IpcChannel};

/// What one role answered to `logging reload`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reload {
    /// Its channel does not answer.
    NotRunning,
    /// The new configuration applies.
    Reloaded,
    /// The role refused the file and keeps its configuration.
    Refused(String),
}

async fn runner(layout: &Layout, principal: &str, name: &str) -> Reload {
    let address = layout.runner_ipc(principal, name);
    let Ok(c) = IpcClient::connect(&address, IpcChannel::RunnerControl).await else {
        return Reload::NotRunning;
    };
    let reply = c
        .call::<_, ControlReply>(&RunnerControl::LoggingReload)
        .await;
    c.close().await;
    match reply {
        Ok(ControlReply::Ok) => Reload::Reloaded,
        Ok(ControlReply::Failed { message }) | Err(message) => Reload::Refused(message),
        Ok(ControlReply::Link { .. }) => {
            Reload::Refused("unexpected answer to logging reload".into())
        }
    }
}

async fn hub(layout: &Layout, principal: &str) -> Reload {
    let Ok(c) = IpcClient::connect(&layout.hub_ipc(principal), IpcChannel::Cli).await else {
        return Reload::NotRunning;
    };
    let reply = c.call::<_, CliReply>(&CliRequest::LoggingReload).await;
    c.close().await;
    match reply {
        Ok(CliReply::Reloaded) => Reload::Reloaded,
        Ok(CliReply::Error(e)) => Reload::Refused(e.message),
        Ok(other) => Reload::Refused(format!("unexpected answer: {other:?}")),
        Err(e) => Reload::Refused(e),
    }
}

async fn broker(layout: &Layout) -> Reload {
    let Ok(c) = IpcClient::connect_broker_admin(&layout.admin_ipc()).await else {
        return Reload::NotRunning;
    };
    let reply = c.call::<_, AdminReply>(&AdminRequest::LoggingReload).await;
    c.close().await;
    match reply {
        Ok(AdminReply::Ok { .. }) => Reload::Reloaded,
        Ok(AdminReply::Failed { message }) | Err(message) => Reload::Refused(message),
        Ok(other) => Reload::Refused(format!("unexpected answer: {other:?}")),
    }
}

/// Sends `logging reload` to every role of `layout`: its runners, the hub of this OS user and
/// the broker, each with its answer.
pub async fn reload_all(layout: &Layout) -> Result<Vec<(String, Reload)>, String> {
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for t in down_targets(layout, &[]) {
        let answer = match &t {
            Target::Runner(name) => (name.clone(), runner(layout, &principal, name).await),
            Target::Hub => ("hub".into(), hub(layout, &principal).await),
            Target::Broker => ("broker".into(), broker(layout).await),
        };
        out.push(answer);
    }
    Ok(out)
}

/// `logging reload`; the exit code is 1 when a role refused the file or no role runs.
pub async fn logging_reload(layout: &Layout) -> Result<i32, String> {
    let answers = reload_all(layout).await?;
    let mut code = 0;
    let mut any = false;
    for (role, answer) in answers {
        match answer {
            Reload::NotRunning => println!("{role}: not running"),
            Reload::Reloaded => {
                any = true;
                println!("{role}: reloaded");
            }
            Reload::Refused(why) => {
                any = true;
                code = 1;
                eprintln!("{role}: {why}");
            }
        }
    }
    if !any {
        eprintln!("no role of this installation runs");
        return Ok(1);
    }
    Ok(code)
}
