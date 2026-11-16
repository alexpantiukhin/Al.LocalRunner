//! A running runner applied the `runner.toml` it read at start or at the last `reload`; when
//! the file on disk is another one, `status` and `doctor` say so and name `reload`.

pub use crate::runner_reload::{disk_differs, hash_text};

use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_proto::{ControlReply, IpcChannel, RunnerControl};

/// The warning for runner `name` when its answer to a status request carries the hash of the
/// text it applied and the file on disk differs; `None` for a runner that does not report a
/// hash (an older build) or whose file is unchanged.
pub fn drift_line(name: &str, text_on_disk: &str, reply: &ControlReply) -> Option<String> {
    match reply {
        ControlReply::Link {
            config_hash: Some(hash),
            ..
        } if disk_differs(text_on_disk, hash) => Some(format!(
            "runner {name} runs with an older runner.toml: localrunner reload"
        )),
        _ => None,
    }
}

async fn runner_status(layout: &Layout, principal: &str, name: &str) -> Option<ControlReply> {
    let c = IpcClient::connect(
        &layout.runner_ipc(principal, name),
        IpcChannel::RunnerControl,
    )
    .await
    .ok()?;
    let reply = c.call::<_, ControlReply>(&RunnerControl::Status).await;
    c.close().await;
    reply.ok()
}

/// One warning per running runner of `layout` whose `runner.toml` changed after it was
/// applied. The file compared is the one the runner finds by default (its own directory, else
/// the template of the configuration directory).
pub async fn lines(layout: &Layout) -> Vec<String> {
    let Ok(principal) = lr_net::current_os_principal() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for name in layout.runner_names() {
        let Some(reply) = runner_status(layout, &principal, &name).await else {
            continue;
        };
        let text = crate::build::runner_config_path(layout, &name, None)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        out.extend(drift_line(&name, &text, &reply));
    }
    out
}
