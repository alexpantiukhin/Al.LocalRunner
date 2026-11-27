//! `runner outbox drop`: close the pending outbox entries of a job on a running runner.

use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_proto::{ControlReply, IpcChannel, RunnerControl};

pub(super) fn pick(names: Vec<String>, runner: Option<String>) -> Result<String, String> {
    match (runner, names.as_slice()) {
        (Some(name), _) => Ok(name),
        (None, [only]) => Ok(only.clone()),
        (None, []) => Err("no runner is set up on this installation".into()),
        (None, _) => Err(format!(
            "several runners here ({}): pass --runner NAME",
            names.join(", ")
        )),
    }
}

/// Asks the runner to close the pending outbox entries of `job` and prints
/// `closed N entries of <job>`. The exit code is 1 when the runner refuses, as it does while
/// the job runs, or does not run.
pub async fn drop_outbox(
    layout: &Layout,
    runner: Option<String>,
    job: String,
) -> Result<i32, String> {
    let name = pick(layout.runner_names(), runner)?;
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    let address = layout.runner_ipc(&principal, &name);
    let Ok(c) = IpcClient::connect(&address, IpcChannel::RunnerControl).await else {
        eprintln!("{name}: not running");
        return Ok(1);
    };
    let reply = c
        .call::<_, ControlReply>(&RunnerControl::DropOutbox { job: job.clone() })
        .await;
    c.close().await;
    match reply {
        Ok(ControlReply::Dropped { closed }) => {
            println!("closed {closed} entries of {job}");
            Ok(0)
        }
        Ok(ControlReply::Failed { message }) | Err(message) => {
            eprintln!("{name}: {message}");
            Ok(1)
        }
        Ok(_) => {
            eprintln!("{name}: this runner is older and cannot drop an outbox: restart it");
            Ok(1)
        }
    }
}
