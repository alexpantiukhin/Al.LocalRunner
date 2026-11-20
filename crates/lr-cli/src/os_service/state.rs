//! The running state of the broker service, read without administrator rights.

use super::{programs, service_name, Os};
use std::process::Command;
use std::time::Duration;

/// How long a question to the OS service manager may take before its answer counts as unknown.
pub const MANAGER_WITHIN: Duration = Duration::from_secs(10);

/// Runs the blocking question `ask` to the service manager (`sc.exe`, `systemctl`, `launchctl`)
/// off the async workers, for at most [`MANAGER_WITHIN`]; `unknown` when it takes longer or
/// cannot be run.
pub async fn ask_manager<T: Send + 'static>(
    unknown: T,
    ask: impl FnOnce() -> T + Send + 'static,
) -> T {
    match tokio::time::timeout(MANAGER_WITHIN, tokio::task::spawn_blocking(ask)).await {
        Ok(Ok(answer)) => answer,
        _ => unknown,
    }
}

/// The state from `sc.exe query` output: `Some(true)` for `RUNNING`, `Some(false)` for another
/// state, `None` when the output has no state line. The line is found by its `STATE` key and,
/// when a localised `sc.exe` translates the key, by position: `TYPE` is the first line whose
/// value starts with a number, the state is the second, and its number 4 means running.
pub fn parse_sc_state(output: &str) -> Option<bool> {
    let numbered: Vec<(&str, u32)> = output
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            Some((key.trim(), value.split_whitespace().next()?.parse().ok()?))
        })
        .collect();
    let (_, code) = numbered
        .iter()
        .find(|(key, _)| *key == "STATE")
        .or_else(|| numbered.get(1))?;
    Some(*code == 4)
}

/// The state from `systemctl is-active` output: `Some(true)` for `active`, `Some(false)` for any
/// other word, `None` for empty output.
pub fn parse_systemctl_active(output: &str) -> Option<bool> {
    let word = output.trim();
    (!word.is_empty()).then(|| word == "active")
}

/// The state from `launchctl print` output: `Some(true)` for `state = running`, `Some(false)` for
/// another state, `None` when there is no `state` line.
pub fn parse_launchctl_state(output: &str) -> Option<bool> {
    output.lines().find_map(|line| {
        let value = line.trim().strip_prefix("state = ")?;
        Some(value.trim() == "running")
    })
}

fn output_of(program: &str, args: &[&str]) -> Option<String> {
    let path = programs::program_path(program).ok()?;
    let out = Command::new(path).args(args).output().ok()?;
    Some(super::console_text::console_text(&out.stdout))
}

/// Whether the broker service is running according to the service manager: `None` when that is
/// not known (not installed, the manager cannot be asked).
pub fn service_running(os: Os) -> Option<bool> {
    named_service_running(os, service_name(os))
}

/// Whether the service `name` (a Windows service, a system unit, a `system/` launchd label) is
/// running, as for [`service_running`].
pub fn named_service_running(os: Os, name: &str) -> Option<bool> {
    match os {
        Os::Windows => parse_sc_state(&output_of("sc.exe", &["query", name])?),
        Os::Linux => parse_systemctl_active(&output_of("systemctl", &["is-active", name])?),
        Os::MacOs => {
            let target = format!("system/{name}");
            parse_launchctl_state(&output_of("launchctl", &["print", &target])?)
        }
    }
}

/// The command that shows the state of the broker service, for an elevated shell.
pub fn status_command(os: Os) -> String {
    named_status_command(os, service_name(os))
}

/// The command that shows the state of the service `name`, for an elevated shell.
pub fn named_status_command(os: Os, name: &str) -> String {
    match os {
        Os::Windows => format!("Get-Service {name}"),
        Os::Linux => format!("sudo systemctl status {name}"),
        Os::MacOs => format!("sudo launchctl print system/{name}"),
    }
}

/// The command that stops the service `name`, for an elevated shell.
pub fn named_stop_command(os: Os, name: &str) -> String {
    match os {
        Os::Windows => format!("Stop-Service {name}"),
        Os::Linux => format!("sudo systemctl stop {name}"),
        Os::MacOs => format!("sudo launchctl kill TERM system/{name}"),
    }
}

/// The command that restarts the broker service, for an elevated shell.
pub fn restart_command(os: Os) -> String {
    named_restart_command(os, service_name(os))
}

/// The command that restarts the service `name`, for an elevated shell.
pub fn named_restart_command(os: Os, name: &str) -> String {
    match os {
        Os::Windows => format!("Restart-Service {name}"),
        Os::Linux => format!("sudo systemctl restart {name}"),
        Os::MacOs => format!("sudo launchctl kickstart -k system/{name}"),
    }
}
