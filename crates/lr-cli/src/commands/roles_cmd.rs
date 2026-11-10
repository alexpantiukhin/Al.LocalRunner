//! `hub run`, `broker run`, `runner run` (roles in the foreground, spec 11.1) and `down` for
//! runners through their local control channel (spec 3.9).

use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use crate::roles::{self, Running};
use lr_proto::{ControlReply, DownMode, IpcChannel, RunnerControl};
use lr_runner::AgentExit;
use std::path::PathBuf;

#[cfg(unix)]
async fn terminated() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut s) => {
            s.recv().await;
        }
        Err(_) => std::future::pending().await,
    }
}

#[cfg(not(unix))]
async fn terminated() {
    std::future::pending().await
}

async fn until_interrupted(running: Running) -> Result<i32, String> {
    let ended = tokio::select! {
        exits = running.wait() => Some(exits),
        _ = tokio::signal::ctrl_c() => None,
        _ = terminated() => None,
    };
    match ended {
        None => Ok(0),
        Some(exits) => {
            let refused = exits.iter().any(|e| matches!(e, AgentExit::Refused(_)));
            for e in exits {
                if let AgentExit::Refused(p) = e {
                    eprintln!("the broker refused this runner: {}", p.message);
                }
            }
            Ok(i32::from(refused))
        }
    }
}

/// `hub run`; `background` when the OS started it at logon.
pub async fn hub_run(layout: &Layout, background: bool) -> Result<i32, String> {
    if background {
        crate::os_service::background_console();
    }
    let running = roles::start_hub(layout, true).await?;
    eprintln!("hub running; Ctrl+C stops it");
    until_interrupted(running).await
}

/// `broker run`; `service` under the OS service manager (Windows: the service host).
pub async fn broker_run(layout: &Layout, background: bool, service: bool) -> Result<i32, String> {
    #[cfg(windows)]
    if service {
        let layout = layout.clone();
        return tokio::task::spawn_blocking(move || crate::os_service::windows_host::run(layout))
            .await
            .map_err(|e| e.to_string())?
            .map(|()| 0);
    }
    if background || service {
        crate::os_service::background_console();
    }
    let running = roles::start_broker(layout, true).await?;
    for a in running.network_addresses() {
        eprintln!("broker listening on {a}");
    }
    eprintln!("broker running; Ctrl+C stops it");
    until_interrupted(running).await
}

/// `runner run`; `background` when the OS started it at logon.
pub async fn runner_run(
    layout: &Layout,
    name: Option<String>,
    count: u32,
    config: Option<PathBuf>,
    background: bool,
) -> Result<i32, String> {
    if background {
        crate::os_service::background_console();
    }
    let running =
        roles::start_runners(layout, name.as_deref(), count, config.as_deref(), true).await?;
    eprintln!("runners running: {}", running.runners.join(", "));
    until_interrupted(running).await
}

/// `down [names] [--drain | --cancel]` for runners of this installation.
pub async fn down(layout: &Layout, names: Vec<String>, cancel: bool) -> Result<i32, String> {
    let names = if names.is_empty() {
        layout.runner_names()
    } else {
        names
    };
    if names.is_empty() {
        return Err("no runners on this installation".into());
    }
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    let mode = if cancel {
        DownMode::Cancel
    } else {
        DownMode::Drain
    };
    let mut code = 0;
    for name in names {
        if let Err(e) = crate::runner_config::check_runner_name(&name) {
            eprintln!("{e}");
            code = 1;
            continue;
        }
        let address = layout.runner_ipc(&principal, &name);
        let reply = match IpcClient::connect(&address, IpcChannel::RunnerControl).await {
            Ok(c) => {
                c.call::<_, ControlReply>(&RunnerControl::Down { mode })
                    .await
            }
            Err(e) => Err(e),
        };
        match reply {
            Ok(ControlReply::Ok) => println!("{name}: stopping ({mode:?})"),
            Ok(ControlReply::Failed { message }) | Err(message) => {
                eprintln!("{name}: {message}");
                code = 1;
            }
        }
    }
    Ok(code)
}
