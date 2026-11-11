//! `down [names] [--drain | --cancel]` (spec 3.9, 11.1): runners through their local control
//! channel, then the hub through its CLI channel, then the broker through its admin channel.
//! Without names every role of this installation that runs stops; runners told to drain finish
//! their job before the broker goes, so the result still reaches it. Ctrl+C ends only that
//! wait: the draining runners and the broker keep running. With `--cancel` the wait is at
//! most 30 s. A broker run by the OS service manager answers that it stays, and nothing waits
//! for the runners before it.

use crate::ipc_client::IpcClient;
use crate::paths::Layout;
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

/// `down [names] [--drain | --cancel]`; the exit code is 1 when a named role does not run, a
/// role refused to stop or the wait for draining runners was interrupted.
pub async fn down(layout: &Layout, names: Vec<String>, cancel: bool) -> Result<i32, String> {
    let explicit = !names.is_empty();
    let targets = down_targets(layout, &names);
    if targets.is_empty() {
        return Err("nothing to stop".into());
    }
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    let mode = down_mode(cancel);
    let service = targets.contains(&Target::Broker) && layout.broker_is_service();
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
                if !service && !stopping.is_empty() {
                    eprintln!(
                        "waiting for {} to {} before the broker stops (Ctrl+C stops waiting)",
                        stopping.join(", "),
                        if cancel { "exit" } else { "finish" }
                    );
                    let addresses: Vec<String> = stopping
                        .iter()
                        .map(|n| layout.runner_ipc(&principal, n))
                        .collect();
                    let addresses = &addresses;
                    let any_answers = move || async move {
                        for a in addresses {
                            if answers(a, IpcChannel::RunnerControl).await {
                                return true;
                            }
                        }
                        false
                    };
                    let timed_out = AtomicBool::new(false);
                    let interrupt = async {
                        match cancel {
                            true => tokio::select! {
                                _ = ctrl_c() => {}
                                _ = tokio::time::sleep(CANCELLED_EXIT_WITHIN) => {
                                    timed_out.store(true, Ordering::Relaxed);
                                }
                            },
                            false => ctrl_c().await,
                        }
                    };
                    if !drained(any_answers, interrupt).await {
                        let why = match timed_out.load(Ordering::Relaxed) {
                            true => format!(
                                "{} did not exit within {} s after the cancel",
                                stopping.join(", "),
                                CANCELLED_EXIT_WITHIN.as_secs()
                            ),
                            false => {
                                format!("stopped waiting: {} keep running", stopping.join(", "))
                            }
                        };
                        eprintln!(
                            "{why}; the broker keeps running: run `localrunner down broker` later"
                        );
                        return Ok(1);
                    }
                }
                match stop_broker(layout).await {
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
