//! Broker administration from the CLI: over the broker's local admin channel only
//! (spec 3.9); `policy check` reads the file itself and needs no broker.

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
