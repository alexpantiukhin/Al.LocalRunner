//! `restart <runners>`: the runners finish their jobs and stop, then the same runners start
//! again, so the user does not chain `down --wait` and `up` by hand.

use crate::commands::{down_cmd, up_cmd};
use crate::paths::Layout;
use std::future::Future;
use std::time::Duration;

/// The names `restart` takes: runners, `local:<name>` or `runner` for every runner.
pub fn runner_names_only(names: &[String]) -> Result<(), String> {
    if names.is_empty() {
        return Err("name the runners to restart, or `runner` for every runner".into());
    }
    match names.iter().find(|n| matches!(n.as_str(), "hub" | "broker" | "all")) {
        Some(role) => Err(format!(
            "restart takes runners only, not `{role}`: stop and start a hub or a broker with `down` and `up`"
        )),
        None => Ok(()),
    }
}

/// Runs `stopped`, and only when it ends with code 0 runs `started`; the code of the one that
/// ran last.
pub async fn restart_with(
    names: &[String],
    stopped: impl Future<Output = Result<i32, String>>,
    started: impl Future<Output = Result<i32, String>>,
) -> Result<i32, String> {
    runner_names_only(names)?;
    match stopped.await? {
        0 => started.await,
        code => Ok(code),
    }
}

/// `restart names [--wait-timeout T]`: a drain that waits for the runners to exit, without a
/// time limit unless `wait_timeout` is given, then `up` of the same names.
pub async fn restart(
    layout: &Layout,
    names: Vec<String>,
    wait_timeout: Option<Duration>,
) -> Result<i32, String> {
    let stopped = down_cmd::down_cli(layout, names.clone(), false, false, Some(wait_timeout));
    let started = up_cmd::up(layout, names.clone());
    restart_with(&names, stopped, started).await
}
