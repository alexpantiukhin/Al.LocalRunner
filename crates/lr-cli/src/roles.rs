//! Starting the roles of this machine in this process (`hub run`, `broker run`, `runner run`):
//! each start builds the role through [`crate::build`] and spawns it.

use crate::broker_admin::TlsListening;
use crate::paths::Layout;
use crate::runner_config::{instance_names, RunnerConfig};
use crate::{admin, build, control};
use lr_abstractions::Listener;
use lr_net::IpcListener;
use lr_obs::{Logger, Metrics};
use lr_runner::AgentExit;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// A role running in this process.
pub struct Running {
    tasks: Vec<JoinHandle<()>>,
    loggers: Vec<Arc<Logger>>,
    tls: Option<Arc<TlsListening>>,
    /// Names of the started runners (empty for hub and broker).
    pub runners: Vec<String>,
    exits: Vec<JoinHandle<AgentExit>>,
    stop_signal: Option<Arc<Notify>>,
    listeners: Vec<Arc<IpcListener>>,
}

impl Running {
    /// The broker's bound network addresses, empty while it does not listen.
    pub fn network_addresses(&self) -> Vec<SocketAddr> {
        self.tls.as_ref().map(|t| t.bound()).unwrap_or_default()
    }

    /// The metric registries of the started roles, one per logger.
    pub fn metrics(&self) -> Vec<Arc<Metrics>> {
        self.loggers.iter().map(|l| l.metrics()).collect()
    }

    /// Waits until the role ends on its own (a runner after `down`, a refused key, a hub or
    /// broker told to stop by `down`).
    pub async fn wait(mut self) -> Vec<AgentExit> {
        let mut out = Vec::new();
        for e in std::mem::take(&mut self.exits) {
            if let Ok(exit) = e.await {
                out.push(exit);
            }
        }
        if out.is_empty() {
            let tasks = &mut self.tasks;
            let all = async {
                for t in tasks.iter_mut() {
                    let _ = t.await;
                }
            };
            match self.stop_signal.clone() {
                Some(stop) => tokio::select! {
                    _ = all => {}
                    _ = stop.notified() => {}
                },
                None => all.await,
            }
        }
        self.stop();
        out
    }

    /// Stops the role at once and flushes its diagnostics.
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        for t in self.tasks.drain(..) {
            t.abort();
        }
        for e in self.exits.drain(..) {
            e.abort();
        }
        for l in self.listeners.drain(..) {
            l.close();
        }
        for l in self.loggers.drain(..) {
            l.shutdown();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Starts the broker with its admin channel.
pub async fn start_broker(layout: &Layout, foreground: bool) -> Result<Running, String> {
    let parts = build::broker(layout, foreground).await?;
    let broker_task = tokio::spawn(parts.broker.run());
    let listeners = vec![parts.admin_ipc.clone(), parts.admin.ipc.clone()];
    let admin_ipc: Arc<dyn Listener> = parts.admin_ipc;
    let stop = parts.admin.stop.clone();
    let admin_task = tokio::spawn(admin::serve(admin_ipc, parts.admin));
    Ok(Running {
        tasks: vec![broker_task, admin_task],
        loggers: vec![parts.logger],
        tls: Some(parts.tls),
        runners: Vec::new(),
        exits: Vec::new(),
        stop_signal: Some(stop),
        listeners,
    })
}

/// Starts the hub of the current OS user.
pub async fn start_hub(layout: &Layout, foreground: bool) -> Result<Running, String> {
    let parts = build::hub(layout, foreground).await?;
    let task = tokio::spawn(parts.hub.run());
    Ok(Running {
        tasks: vec![task],
        loggers: vec![parts.logger],
        tls: None,
        runners: Vec::new(),
        exits: Vec::new(),
        stop_signal: None,
        listeners: vec![parts.ipc],
    })
}

/// The base name of the runners to start: `name`, else the only runner of the installation,
/// else `name` of the `runner.toml` template, else the host name.
pub fn runner_base_name(layout: &Layout, name: Option<&str>, config: Option<&Path>) -> String {
    if let Some(n) = name {
        return n.to_string();
    }
    let existing = layout.runner_names();
    if let [only] = existing.as_slice() {
        return only.clone();
    }
    let host = crate::facts::host_name();
    let template = config
        .map(Path::to_path_buf)
        .or_else(|| Some(layout.config_file("runner.toml")).filter(|p| p.exists()));
    template
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| {
            RunnerConfig::parse(
                &t,
                None,
                &host,
                layout.root.clone(),
                crate::host::resources(),
            )
            .ok()
        })
        .map_or(host, |c| c.settings.name)
}

/// Starts `count` runners named from `name` (see [`runner_base_name`] and
/// [`instance_names`]), each with its own state directory and control channel. A runner
/// running as root or administrator without `allow_elevated` is refused.
pub async fn start_runners(
    layout: &Layout,
    name: Option<&str>,
    count: u32,
    config: Option<&Path>,
    foreground: bool,
) -> Result<Running, String> {
    let base = runner_base_name(layout, name, config);
    let mut running = Running {
        tasks: Vec::new(),
        loggers: Vec::new(),
        tls: None,
        runners: Vec::new(),
        exits: Vec::new(),
        stop_signal: None,
        listeners: Vec::new(),
    };
    for n in instance_names(&base, count)? {
        let parts = build::runner(layout, &n, config, foreground).await?;
        for p in &parts.problems {
            eprintln!("runner {n}: {p}");
        }
        let control = parts.agent.control();
        running.listeners.push(parts.control.clone());
        let listener: Arc<dyn Listener> = parts.control;
        let reload = build::logging_reloader(layout, &parts.logger);
        running
            .tasks
            .push(tokio::spawn(control::serve(listener, control, reload)));
        running.exits.push(tokio::spawn(parts.agent.run()));
        running.loggers.push(parts.logger);
        running.runners.push(n);
    }
    Ok(running)
}
