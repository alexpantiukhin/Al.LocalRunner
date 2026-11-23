//! `reload`: running roles take their settings files again without a restart — runners
//! `runner.toml`, the hub `hub-policy.toml`, the broker `policy.toml`.

use super::down_cmd::{down_targets, Target};
use super::RoleArg;
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_proto::{
    AdminReply, AdminRequest, CliReply, CliRequest, ControlReply, IpcChannel, RunnerControl,
};

/// What one role answered to `reload`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Its channel does not answer.
    NotRunning,
    /// The new file applies; the settings that need a restart are named.
    Reloaded {
        /// Settings that differ and keep their running value until a restart.
        restart_required: Vec<String>,
    },
    /// The role refused the file and keeps what it had.
    Refused(String),
}

async fn runner(layout: &Layout, principal: &str, name: &str) -> Outcome {
    let address = layout.runner_ipc(principal, name);
    let Ok(c) = IpcClient::connect(&address, IpcChannel::RunnerControl).await else {
        use crate::os_service::{runner_service, Os};
        let os = Os::current();
        let n = name.to_string();
        let service = tokio::task::spawn_blocking(move || {
            runner_service::installed(os, &n).then(|| runner_service::running(os, &n))
        })
        .await
        .ok()
        .flatten();
        return match service {
            Some(running) => Outcome::Refused(runner_service::unreachable_line(
                os, name, running, "reload",
            )),
            None => Outcome::NotRunning,
        };
    };
    let reply = c.call::<_, ControlReply>(&RunnerControl::Reload).await;
    c.close().await;
    match reply {
        Ok(ControlReply::Reloaded { restart_required }) => Outcome::Reloaded { restart_required },
        Ok(ControlReply::Failed { message }) | Err(message) => Outcome::Refused(message),
        Ok(_) => Outcome::Refused("this runner is older and cannot reload: restart it".into()),
    }
}

async fn hub(layout: &Layout, principal: &str) -> Outcome {
    let Ok(c) = IpcClient::connect(&layout.hub_ipc(principal), IpcChannel::Cli).await else {
        return Outcome::NotRunning;
    };
    let reply = c.call::<_, CliReply>(&CliRequest::PolicyReload).await;
    c.close().await;
    match reply {
        Ok(CliReply::Reloaded) => Outcome::Reloaded {
            restart_required: Vec::new(),
        },
        Ok(CliReply::Error(e)) => Outcome::Refused(e.message),
        Ok(other) => Outcome::Refused(format!("unexpected answer: {other:?}")),
        Err(e) => Outcome::Refused(e),
    }
}

async fn broker(layout: &Layout) -> Outcome {
    let Ok(c) = IpcClient::connect_broker_admin(&layout.admin_ipc()).await else {
        return Outcome::NotRunning;
    };
    let reply = c.call::<_, AdminReply>(&AdminRequest::PolicyReload).await;
    c.close().await;
    match reply {
        Ok(AdminReply::Ok { .. }) => Outcome::Reloaded {
            restart_required: Vec::new(),
        },
        Ok(AdminReply::Failed { message }) | Err(message) => Outcome::Refused(message),
        Ok(other) => Outcome::Refused(format!("unexpected answer: {other:?}")),
    }
}

fn wanted(target: &Target, role: Option<RoleArg>) -> bool {
    matches!(
        (role, target),
        (None | Some(RoleArg::All), _)
            | (Some(RoleArg::Runner), Target::Runner(_))
            | (Some(RoleArg::Hub), Target::Hub)
            | (Some(RoleArg::Broker), Target::Broker)
    )
}

/// Sends `reload` to one role of `layout` and returns its answer.
pub async fn reload_one(layout: &Layout, target: &Target) -> Result<Outcome, String> {
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    Ok(reload_with(layout, &principal, target).await)
}

async fn reload_with(layout: &Layout, principal: &str, target: &Target) -> Outcome {
    match target {
        Target::Runner(n) => runner(layout, principal, n).await,
        Target::Hub => hub(layout, principal).await,
        Target::Broker => broker(layout).await,
    }
}

/// Sends `reload` to the roles selected by `role` and `name` (a runner, or `hub` or `broker`);
/// without them every role of `layout`. One answer per role.
pub async fn reload_all(
    layout: &Layout,
    role: Option<RoleArg>,
    name: Option<String>,
) -> Result<Vec<(String, Outcome)>, String> {
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    let names: Vec<String> = name.into_iter().collect();
    let mut out = Vec::new();
    for t in down_targets(layout, &names)
        .into_iter()
        .filter(|t| wanted(t, role))
    {
        let who = match &t {
            Target::Runner(n) => n.clone(),
            Target::Hub => "hub".into(),
            Target::Broker => "broker".into(),
        };
        out.push((who, reload_with(layout, &principal, &t).await));
    }
    Ok(out)
}

/// `reload`: one line per role, and `needs a restart: <setting>` for each setting that differs
/// but cannot change while the role runs. The exit code is 1 when a role refused its file or
/// none of the selected roles runs.
pub async fn reload(
    layout: &Layout,
    role: Option<RoleArg>,
    name: Option<String>,
) -> Result<i32, String> {
    let (mut code, mut any) = (0, false);
    for (who, answer) in reload_all(layout, role, name).await? {
        match answer {
            Outcome::NotRunning => println!("{who}: not running"),
            Outcome::Reloaded { restart_required } => {
                any = true;
                println!("{who}: reloaded");
                for setting in restart_required {
                    println!("{who}: needs a restart: {setting}");
                }
            }
            Outcome::Refused(why) => {
                any = true;
                code = 1;
                eprintln!("{who}: {why}");
            }
        }
    }
    if !any {
        eprintln!("no selected role of this installation runs");
        return Ok(1);
    }
    Ok(code)
}
