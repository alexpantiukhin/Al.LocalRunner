//! Broker administration from the CLI: over the broker's local admin channel only
//! (spec 3.9); `policy check` reads the file itself and needs no broker.

use super::confirm::{proceed, Console, Stdio};
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_broker::policy::{check, load};
use lr_proto::{AdminReply, AdminRequest};
use std::path::PathBuf;

/// `policy check [file]`: errors stop the file, shadowed rules and the like are warnings.
pub async fn policy_check(layout: &Layout, file: Option<PathBuf>) -> Result<i32, String> {
    let path = file.unwrap_or_else(|| layout.config_file("policy.toml"));
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    match load(&text) {
        Err(e) => {
            eprintln!("{e}");
            Ok(1)
        }
        Ok(policy) => {
            for w in check(&policy, &crate::host::resources()) {
                println!("warning: {w}");
            }
            println!("{}: valid", path.display());
            Ok(0)
        }
    }
}

/// `runner remove` or `client remove` as the command line runs it (see [`remove_asked`]).
pub async fn remove_cli(layout: &Layout, req: AdminRequest, yes: bool) -> Result<i32, String> {
    let mut stdio = Stdio::new();
    let mut console = stdio.console();
    remove_asked(layout, req, yes, &mut console).await
}

/// Sends the removal `req` (`RunnerRemove` or `ClientRemove`) after asking, because it revokes
/// a key (`yes` answers); a refusal or a "no" ends before the broker is reached.
pub async fn remove_asked(
    layout: &Layout,
    req: AdminRequest,
    yes: bool,
    console: &mut Console<'_>,
) -> Result<i32, String> {
    let (kind, name) = match &req {
        AdminRequest::RunnerRemove { name } => ("runner", name),
        AdminRequest::ClientRemove { name } => ("client", name),
        _ => return simple(layout, req).await,
    };
    let action = format!("{kind} remove revokes the key of {kind} {name}");
    let question = format!("remove {kind} {name} and revoke its key? It needs a new join code.");
    if !proceed(&action, &question, yes, console)? {
        return Ok(1);
    }
    simple(layout, req).await
}

/// Sends one admin command to the broker of this installation and prints the answer.
pub async fn simple(layout: &Layout, req: AdminRequest) -> Result<i32, String> {
    let client = IpcClient::connect_broker_admin(&layout.admin_ipc())
        .await
        .map_err(|e| format!("the broker admin channel is not reachable: {e}"))?;
    let reply: AdminReply = client.call(&req).await?;
    client.close().await;
    match reply {
        AdminReply::Ok { warnings } => {
            for w in warnings {
                println!("warning: {w}");
            }
            println!("ok");
            Ok(0)
        }
        AdminReply::JoinCode {
            address,
            code,
            fingerprint,
            expires_at,
        } => {
            println!("address:     {address}");
            println!("code:        {code}");
            println!("fingerprint: {fingerprint}");
            println!("expires at:  {} (unix ms)", expires_at.0);
            Ok(0)
        }
        AdminReply::Names { names } => {
            for n in names {
                println!("{n}");
            }
            Ok(0)
        }
        AdminReply::Failed { message } => {
            eprintln!("{message}");
            Ok(1)
        }
    }
}
