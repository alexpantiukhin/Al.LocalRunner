//! `down [names] [--drain | --cancel]` (spec 3.9, 11.1): runners through their local control
//! channel, then the hub through its CLI channel, then the broker through its admin channel.
//! Without names every role of this installation that runs stops; runners told to drain finish
//! their job before the broker goes, so the result still reaches it. A broker run by the OS
//! service manager stays.

use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_proto::{AdminReply, AdminRequest, CliReply, CliRequest, ControlReply, DownMode};
use lr_proto::{IpcChannel, RunnerControl};
use std::time::Duration;

const POLL: Duration = Duration::from_millis(200);
const ROLE_GONE_WITHIN: Duration = Duration::from_secs(15);

/// What `down` stops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A runner of this installation.
    Runner(String),
    /// The hub of this OS user.
    Hub,
    /// The broker of this installation.
    Broker,
}

/// The targets of `down names`, runners first; without names every runner of the
/// installation, the hub and the broker.
pub fn down_targets(layout: &Layout, names: &[String]) -> Vec<Target> {
    let mut out: Vec<Target> = if names.is_empty() {
        let mut all: Vec<Target> = layout
            .runner_names()
            .into_iter()
            .map(Target::Runner)
            .collect();
        all.extend([Target::Hub, Target::Broker]);
        all
    } else {
        names
            .iter()
            .map(|n| match n.as_str() {
                "hub" => Target::Hub,
                "broker" => Target::Broker,
                _ => Target::Runner(n.clone()),
            })
            .collect()
    };
    out.sort_by_key(|t| match t {
        Target::Runner(_) => 0,
        Target::Hub => 1,
        Target::Broker => 2,
    });
    out.dedup();
    out
}

async fn until_gone(address: &str, channel: IpcChannel, within: Option<Duration>) -> bool {
    let start = tokio::time::Instant::now();
    loop {
        match IpcClient::connect(address, channel).await {
            Err(_) => return true,
            Ok(c) => c.close().await,
        }
        if within.is_some_and(|w| start.elapsed() >= w) {
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
}

async fn runner(
    layout: &Layout,
    principal: &str,
    name: &str,
    mode: DownMode,
) -> Result<bool, String> {
    crate::runner_config::check_runner_name(name)?;
    let address = layout.runner_ipc(principal, name);
    let Ok(c) = IpcClient::connect(&address, IpcChannel::RunnerControl).await else {
        return Ok(false);
    };
    match c
        .call::<_, ControlReply>(&RunnerControl::Down { mode })
        .await?
    {
        ControlReply::Ok => Ok(true),
        ControlReply::Failed { message } => Err(message),
    }
}

async fn hub(layout: &Layout, principal: &str) -> Result<bool, String> {
    let address = layout.hub_ipc(principal);
    let Ok(c) = IpcClient::connect(&address, IpcChannel::Cli).await else {
        return Ok(false);
    };
    match c.call::<_, CliReply>(&CliRequest::Shutdown).await {
        Ok(CliReply::Stopping) | Err(_) => {}
        Ok(other) => return Err(format!("unexpected answer: {other:?}")),
    }
    c.close().await;
    match until_gone(&address, IpcChannel::Cli, Some(ROLE_GONE_WITHIN)).await {
        true => Ok(true),
        false => Err("still answers after it was told to stop".into()),
    }
}

async fn broker(layout: &Layout) -> Result<Option<Vec<String>>, String> {
    let address = layout.admin_ipc();
    let Ok(c) = IpcClient::connect_broker_admin(&address).await else {
        return Ok(None);
    };
    let warnings = match c.call::<_, AdminReply>(&AdminRequest::Shutdown).await? {
        AdminReply::Ok { warnings } => warnings,
        AdminReply::Failed { message } => return Err(message),
        other => return Err(format!("unexpected answer: {other:?}")),
    };
    c.close().await;
    if warnings.is_empty() {
        let gone = async {
            let start = tokio::time::Instant::now();
            while IpcClient::connect_broker_admin(&address).await.is_ok() {
                if start.elapsed() >= ROLE_GONE_WITHIN {
                    return false;
                }
                tokio::time::sleep(POLL).await;
            }
            true
        };
        if !gone.await {
            return Err("still answers after it was told to stop".into());
        }
    }
    Ok(Some(warnings))
}

/// `down [names] [--drain | --cancel]`; the exit code is 1 when a named role does not run or a
/// role refused to stop.
pub async fn down(layout: &Layout, names: Vec<String>, cancel: bool) -> Result<i32, String> {
    let explicit = !names.is_empty();
    let targets = down_targets(layout, &names);
    if targets.is_empty() {
        return Err("nothing to stop".into());
    }
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    let mode = if cancel {
        DownMode::Cancel
    } else {
        DownMode::Drain
    };
    let mut code = 0;
    let not_running = |what: &str, code: &mut i32| {
        if explicit {
            eprintln!("{what}: not running");
            *code = 1;
        } else {
            println!("{what}: not running");
        }
    };
    let mut stopping = Vec::new();
    for t in &targets {
        match t {
            Target::Runner(name) => match runner(layout, &principal, name, mode).await {
                Ok(true) => {
                    println!("{name}: stopping ({mode:?})");
                    stopping.push(name.clone());
                }
                Ok(false) => not_running(name, &mut code),
                Err(e) => {
                    eprintln!("{name}: {e}");
                    code = 1;
                }
            },
            Target::Hub => match hub(layout, &principal).await {
                Ok(true) => println!("hub: stopped"),
                Ok(false) => not_running("hub", &mut code),
                Err(e) => {
                    eprintln!("hub: {e}");
                    code = 1;
                }
            },
            Target::Broker => {
                if !stopping.is_empty() {
                    eprintln!(
                        "waiting for {} to finish before the broker stops (Ctrl+C stops waiting)",
                        stopping.join(", ")
                    );
                    for name in &stopping {
                        let address = layout.runner_ipc(&principal, name);
                        until_gone(&address, IpcChannel::RunnerControl, None).await;
                    }
                }
                match broker(layout).await {
                    Ok(Some(warnings)) if warnings.is_empty() => println!("broker: stopped"),
                    Ok(Some(warnings)) => {
                        for w in warnings {
                            println!("broker: {w}");
                        }
                    }
                    Ok(None) => not_running("broker", &mut code),
                    Err(e) => {
                        eprintln!("broker: {e}");
                        code = 1;
                    }
                }
            }
        }
    }
    Ok(code)
}
