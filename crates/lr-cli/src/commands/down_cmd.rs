//! `down [names] [--drain | --cancel]` (spec 3.9, 11.1): runners through their local control
//! channel, then the hub through its CLI channel, then the broker through its admin channel.
//! Without names every role of this installation that runs stops; runners told to drain finish
//! their job before the broker goes, so the result still reaches it. Ctrl+C ends only that
//! wait: the draining runners and the broker keep running. With `--cancel` the wait is at
//! most 30 s, also before a broker run by the OS service manager, which answers that it stays.

use super::confirm::{proceed, Console, Stdio};
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use async_trait::async_trait;
use lr_proto::{AdminReply, AdminRequest, CliReply, CliRequest, ControlReply, DownMode};
use lr_proto::{IpcChannel, RunnerControl};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const POLL: Duration = Duration::from_millis(200);
const ROLE_GONE_WITHIN: Duration = Duration::from_secs(15);
const CANCELLED_EXIT_WITHIN: Duration = Duration::from_secs(30);

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

async fn answers(address: &str, channel: IpcChannel) -> bool {
    match IpcClient::connect(address, channel).await {
        Err(_) => false,
        Ok(c) => {
            c.close().await;
            true
        }
    }
}

async fn until_gone(address: &str, channel: IpcChannel, within: Duration) -> bool {
    let start = tokio::time::Instant::now();
    while answers(address, channel).await {
        if start.elapsed() >= within {
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
    true
}

/// Polls `still_running` until it says no; `false` when `interrupt` came first.
pub async fn drained<F, Fut>(mut still_running: F, interrupt: impl Future<Output = ()>) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let all = async {
        while still_running().await {
            tokio::time::sleep(POLL).await;
        }
    };
    tokio::select! {
        _ = all => true,
        _ = interrupt => false,
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
    match until_gone(&address, IpcChannel::Cli, ROLE_GONE_WITHIN).await {
        true => Ok(true),
        false => Err("still answers after it was told to stop".into()),
    }
}

/// Tells the broker of `layout` to stop and waits until neither its IPC nor its admin channel
/// answers. `None`
/// when no broker answers; otherwise the broker's warnings (a broker run as an OS service
/// keeps running and says so). A broker that stops before its answer leaves counts as
/// stopping.
pub async fn stop_broker(layout: &Layout) -> Result<Option<Vec<String>>, String> {
    let address = layout.admin_ipc();
    let Ok(c) = IpcClient::connect_broker_admin(&address).await else {
        return Ok(None);
    };
    let warnings = match c.call::<_, AdminReply>(&AdminRequest::Shutdown).await {
        Ok(AdminReply::Ok { warnings }) => warnings,
        Ok(AdminReply::Failed { message }) => return Err(message),
        Ok(other) => return Err(format!("unexpected answer: {other:?}")),
        Err(_) => Vec::new(),
    };
    c.close().await;
    if warnings.is_empty() {
        let start = tokio::time::Instant::now();
        while super::up_cmd::broker_answers(layout).await {
            if start.elapsed() >= ROLE_GONE_WITHIN {
                return Err("still answers after it was told to stop".into());
            }
            tokio::time::sleep(POLL).await;
        }
    }
    Ok(Some(warnings))
}

/// How the wait for stopping runners ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waited {
    /// Every runner went.
    Gone,
    /// After `--cancel` a runner still answered 30 s later.
    TimedOut,
    /// Ctrl+C came first.
    Interrupted,
}

/// Waits until `still_running` says no: with `cancel` at most 30 s, otherwise until
/// `interrupt`.
pub async fn wait_for_runners<F, Fut>(
    still_running: F,
    cancel: bool,
    interrupt: impl Future<Output = ()>,
) -> Waited
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let timed_out = AtomicBool::new(false);
    let stop = async {
        match cancel {
            true => tokio::select! {
                _ = interrupt => {}
                _ = tokio::time::sleep(CANCELLED_EXIT_WITHIN) => {
                    timed_out.store(true, Ordering::Relaxed);
                }
            },
            false => interrupt.await,
        }
    };
    match drained(still_running, stop).await {
        true => Waited::Gone,
        false if timed_out.load(Ordering::Relaxed) => Waited::TimedOut,
        false => Waited::Interrupted,
    }
}

async fn ctrl_c() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// What runners are told: `--cancel` cancels the current job, otherwise (`--drain` or no
/// flag) they finish it.
pub fn down_mode(cancel: bool) -> DownMode {
    if cancel {
        DownMode::Cancel
    } else {
        DownMode::Drain
    }
}

/// How `down` reaches the roles of an installation.
#[async_trait]
pub trait Roles: Send + Sync {
    /// Tells runner `name` to stop in `mode`; `false` when it does not run.
    async fn stop_runner(&self, name: &str, mode: DownMode) -> Result<bool, String>;
    /// Whether runner `name` still answers on its control channel.
    async fn runner_answers(&self, name: &str) -> bool;
    /// Stops the hub; `false` when it does not run.
    async fn stop_hub(&self) -> Result<bool, String>;
    /// Stops the broker as [`stop_broker`] does.
    async fn stop_broker(&self) -> Result<Option<Vec<String>>, String>;
    /// Whether the broker runs under the OS service manager.
    fn broker_is_service(&self) -> bool;
}

/// The roles of `layout` of OS principal `principal`, over their local channels.
pub struct IpcRoles<'a> {
    /// The installation.
    pub layout: &'a Layout,
    /// The OS principal the runner and hub channels are named after.
    pub principal: String,
}

#[async_trait]
impl Roles for IpcRoles<'_> {
    async fn stop_runner(&self, name: &str, mode: DownMode) -> Result<bool, String> {
        runner(self.layout, &self.principal, name, mode).await
    }

    async fn runner_answers(&self, name: &str) -> bool {
        let address = self.layout.runner_ipc(&self.principal, name);
        answers(&address, IpcChannel::RunnerControl).await
    }

    async fn stop_hub(&self) -> Result<bool, String> {
        hub(self.layout, &self.principal).await
    }

    async fn stop_broker(&self) -> Result<Option<Vec<String>>, String> {
        stop_broker(self.layout).await
    }

    fn broker_is_service(&self) -> bool {
        self.layout.broker_is_service()
    }
}

/// `down [names] [--drain | --cancel]`; the exit code is 1 when a named role does not run, a
/// role refused to stop or the wait for draining runners was interrupted.
pub async fn down(layout: &Layout, names: Vec<String>, cancel: bool) -> Result<i32, String> {
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    let roles = IpcRoles { layout, principal };
    down_with(layout, names, cancel, &roles, ctrl_c()).await
}

/// `down` as the command line runs it: `--cancel` asks first (see [`down_asked`]).
pub async fn down_cli(
    layout: &Layout,
    names: Vec<String>,
    cancel: bool,
    yes: bool,
) -> Result<i32, String> {
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    let roles = IpcRoles { layout, principal };
    let mut stdio = Stdio::new();
    let mut console = stdio.console();
    down_asked(layout, names, cancel, yes, &roles, ctrl_c(), &mut console).await
}

/// [`down_with`] that, with `cancel`, first asks (`yes` answers it) because running jobs are
/// killed; a refusal or a "no" ends with code 1 before any role is told to stop. Without
/// `cancel` it never asks.
pub async fn down_asked(
    layout: &Layout,
    names: Vec<String>,
    cancel: bool,
    yes: bool,
    roles: &dyn Roles,
    interrupt: impl Future<Output = ()>,
    console: &mut Console<'_>,
) -> Result<i32, String> {
    if cancel
        && !proceed(
            "down --cancel kills the running jobs",
            "cancel the running jobs and stop the roles? The jobs are lost.",
            yes,
            console,
        )?
    {
        return Ok(1);
    }
    down_with(layout, names, cancel, roles, interrupt).await
}

/// `down` of the roles `roles` reaches; `interrupt` (Ctrl+C) ends the wait for the runners.
/// Before the broker stops, the runners told to stop are waited for: when they drain (so their
/// results still reach the broker) and after `--cancel` at most 30 s, also when the broker is
/// an OS service that stays (spec 10.5). A runner still answering ends `down` with code 1 and
/// leaves the broker running.
pub async fn down_with(
    layout: &Layout,
    names: Vec<String>,
    cancel: bool,
    roles: &dyn Roles,
    interrupt: impl Future<Output = ()>,
) -> Result<i32, String> {
    let explicit = !names.is_empty();
    let targets = down_targets(layout, &names);
    if targets.is_empty() {
        return Err("nothing to stop".into());
    }
    let mode = down_mode(cancel);
    let service = targets.contains(&Target::Broker) && roles.broker_is_service();
    let mut interrupt = Some(interrupt);
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
            Target::Runner(name) => match roles.stop_runner(name, mode).await {
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
            Target::Hub => match roles.stop_hub().await {
                Ok(true) => println!("hub: stopped"),
                Ok(false) => not_running("hub", &mut code),
                Err(e) => {
                    eprintln!("hub: {e}");
                    code = 1;
                }
            },
            Target::Broker => {
                if !stopping.is_empty() && (cancel || !service) {
                    eprintln!(
                        "waiting for {} to {} before the broker stops (Ctrl+C stops waiting)",
                        stopping.join(", "),
                        if cancel { "exit" } else { "finish" }
                    );
                    let names = &stopping;
                    let any_answers = move || async move {
                        for n in names {
                            if roles.runner_answers(n).await {
                                return true;
                            }
                        }
                        false
                    };
                    let waited = match interrupt.take() {
                        Some(i) => wait_for_runners(any_answers, cancel, i).await,
                        None => {
                            let never = std::future::pending::<()>();
                            wait_for_runners(any_answers, cancel, never).await
                        }
                    };
                    if waited != Waited::Gone {
                        let why = match waited {
                            Waited::TimedOut => format!(
                                "{} did not exit within {} s after the cancel",
                                stopping.join(", "),
                                CANCELLED_EXIT_WITHIN.as_secs()
                            ),
                            _ => {
                                format!("stopped waiting: {} keep running", stopping.join(", "))
                            }
                        };
                        eprintln!(
                            "{why}; the broker keeps running: run `localrunner down broker` later"
                        );
                        return Ok(1);
                    }
                }
                match roles.stop_broker().await {
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
