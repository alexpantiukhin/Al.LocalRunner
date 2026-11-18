//! Starting the roles of this machine in this process (`hub run`, `broker run`, `runner run`):
//! each start builds the role through [`crate::build`] and spawns it.

use crate::broker_admin::TlsListening;
use crate::paths::Layout;
use crate::runner_config::{instance_names, RunnerConfig};
use crate::{admin, build, control};
use lr_abstractions::Listener;
use lr_broker::{BusEvent, BusEvents};
use lr_net::IpcListener;
use lr_obs::{Logger, Metrics};
use lr_runner::AgentExit;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{broadcast, Notify};
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
    events: Option<BusEvents>,
}

impl Running {
    /// Subscribes to the broker's bus events (an order queued, a runner online after its hello)
    /// from now on, for callers that wait on what the broker did: subscribe before the action.
    /// `None` for a hub or runners.
    pub fn broker_events(&self) -> Option<broadcast::Receiver<BusEvent>> {
        self.events.as_ref().map(BusEvents::subscribe)
    }

    /// The broker's bound network addresses, empty while it does not listen.
    pub fn network_addresses(&self) -> Vec<SocketAddr> {
        self.tls.as_ref().map(|t| t.bound()).unwrap_or_default()
    }

    /// The metric registries of the started roles, one per logger.
    pub fn metrics(&self) -> Vec<Arc<Metrics>> {
        self.loggers.iter().map(|l| l.metrics()).collect()
    }

    /// The kinds of the log sinks of the started roles.
    pub fn sink_kinds(&self) -> Vec<lr_obs::queue::SinkKind> {
        self.loggers.iter().flat_map(|l| l.sink_kinds()).collect()
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

    /// Stops the role like [`Running::stop`] and waits until its own tasks have ended, so
    /// what they held is let go; for callers that reuse the role's state or addresses next.
    pub async fn stopped(mut self) {
        let tasks = std::mem::take(&mut self.tasks);
        let exits = std::mem::take(&mut self.exits);
        let listeners = std::mem::take(&mut self.listeners);
        for t in &tasks {
            t.abort();
        }
        for e in &exits {
            e.abort();
        }
        self.halt();
        for l in &listeners {
            l.closed().await;
        }
        for t in tasks {
            let _ = t.await;
        }
        for e in exits {
            let _ = e.await;
        }
    }

    /// Writes out the diagnostics the role's loggers hold so far.
    pub fn flush(&self) {
        for l in &self.loggers {
            l.flush();
        }
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
    let events = parts.broker.events();
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
        events: Some(events),
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
        events: None,
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
    if layout.job_root.is_some() && count != 1 {
        return Err("--job-root serves one runner: use --count 1".into());
    }
    let base = runner_base_name(layout, name, config);
    let mut running = Running {
        tasks: Vec::new(),
        loggers: Vec::new(),
        tls: None,
        runners: Vec::new(),
        exits: Vec::new(),
        stop_signal: None,
        listeners: Vec::new(),
        events: None,
    };
    for n in instance_names(&base, count)? {
        let parts = build::runner(layout, &n, config, foreground).await?;
        for p in &parts.problems {
            eprintln!("runner {n}: {p}");
        }
        let control = parts.agent.control();
        if let Some(lock) = crate::upgrade_lock::blocking_here(layout) {
            eprintln!(
                "runner {n}: {}: starting in drain, it leaves once idle",
                crate::upgrade_lock::describe(&lock)
            );
            control.drain(lr_proto::DrainReason::Lock).await;
        }
        running.listeners.push(parts.control.clone());
        let listener: Arc<dyn Listener> = parts.control;
        let reload = build::logging_reloader(layout, &parts.logger);
        running.tasks.push(tokio::spawn(control::serve(
            listener,
            control,
            reload,
            parts.reload.clone(),
        )));
        running.exits.push(tokio::spawn(parts.agent.run()));
        running.loggers.push(parts.logger);
        running.runners.push(n);
    }
    Ok(running)
}
