//! `runner cache clean`: remove the named caches of a running runner to free its disk.

use super::runner_outbox_cmd::pick;
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_proto::{ControlReply, IpcChannel, RunnerControl};

/// The line `runner cache clean` prints: what was removed, or with `dry_run` what would be.
pub fn cleaned_line(name: &str, entries: u64, bytes: u64, dry_run: bool) -> String {
    let verb = if dry_run { "would remove" } else { "removed" };
    format!(
        "{name}: {verb} {entries} cache {} ({} MB)",
        if entries == 1 { "entry" } else { "entries" },
        bytes / (1024 * 1024)
    )
}

/// Asks the runner to remove its caches unused for `older_than_days` (all of them without it),
/// except the ones a running job uses. The exit code is 1 when the runner does not run or refuses.
pub async fn clean(
    layout: &Layout,
    runner: Option<String>,
    older_than_days: Option<u64>,
    dry_run: bool,
) -> Result<i32, String> {
    let name = pick(layout.runner_names(), runner)?;
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    let address = layout.runner_ipc(&principal, &name);
    let Ok(c) = IpcClient::connect(&address, IpcChannel::RunnerControl).await else {
        eprintln!("{name}: not running");
        return Ok(1);
    };
    let reply = c
        .call::<_, ControlReply>(&RunnerControl::CleanCache {
            older_than_days,
            dry_run,
        })
        .await;
    c.close().await;
    match reply {
        Ok(ControlReply::Cleaned { entries, bytes }) => {
            println!("{}", cleaned_line(&name, entries, bytes, dry_run));
            Ok(0)
        }
        Ok(ControlReply::Failed { message }) | Err(message) => {
            eprintln!("{name}: {message}");
            Ok(1)
        }
        Ok(_) => {
            eprintln!("{name}: this runner is older and cannot clean its cache: restart it");
            Ok(1)
        }
    }
}
