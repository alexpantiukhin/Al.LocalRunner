//! `hub run`, `broker run`, `runner run`: roles in the foreground (spec 11.1).

use crate::os_service::service_run::{self, ServiceEnd};
use crate::paths::Layout;
use crate::roles::{self, Running};
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
        return tokio::task::spawn_blocking(move || {
            crate::os_service::windows_host::run(
                layout,
                crate::os_service::windows_host::Hosted::Broker,
            )
        })
        .await
        .map_err(|e| e.to_string())?
        .map(|()| 0);
    }
    if background || service {
        crate::os_service::background_console();
    }
    if service {
        let stop = async {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminated() => {}
            }
        };
        return Ok(match service_run::serve(layout, || {}, stop).await {
            ServiceEnd::Stopped => 0,
            ServiceEnd::NotStarted | ServiceEnd::Ended => 1,
        });
    }
    let running = roles::start_broker(layout, true).await?;
    for a in running.network_addresses() {
        eprintln!("broker listening on {a}");
    }
    eprintln!("broker running; Ctrl+C stops it");
    until_interrupted(running).await
}

/// How `runner run` was started.
#[derive(Debug, Clone, Default)]
pub struct RunnerStart {
    /// The OS started it at logon.
    pub background: bool,
    /// The OS service manager started it (`autostart on runner <name> --service`).
    pub service: bool,
    /// The OS user the installation belongs to, when the service account is another one.
    pub owner: Option<String>,
}

/// `runner run`; see [`RunnerStart`] for how it was started. Under the service manager it runs
/// the one runner `name` until the manager stops it (Windows: the service host).
pub async fn runner_run(
    layout: &Layout,
    name: Option<String>,
    count: u32,
    config: Option<PathBuf>,
    how: RunnerStart,
) -> Result<i32, String> {
    if let Some(owner) = how.owner {
        crate::doctor::set_installation_owner(owner);
    }
    if how.service {
        let name = name.ok_or("--service runs one named runner: --name <name>")?;
        #[cfg(windows)]
        {
            let layout = layout.clone();
            let hosted = crate::os_service::windows_host::Hosted::Runner(name);
            return tokio::task::spawn_blocking(move || {
                crate::os_service::windows_host::run(layout, hosted)
            })
            .await
            .map_err(|e| e.to_string())?
            .map(|()| 0);
        }
        #[cfg(not(windows))]
        {
            crate::os_service::background_console();
            let stop = async {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminated() => {}
                }
            };
            return Ok(
                match service_run::serve_runner(layout, &name, || {}, stop).await {
                    ServiceEnd::Stopped => 0,
                    ServiceEnd::NotStarted | ServiceEnd::Ended => 1,
                },
            );
        }
    }
    if how.background {
        crate::os_service::background_console();
    }
    let running =
        roles::start_runners(layout, name.as_deref(), count, config.as_deref(), true).await?;
    eprintln!("runners running: {}", running.runners.join(", "));
    until_interrupted(running).await
}
