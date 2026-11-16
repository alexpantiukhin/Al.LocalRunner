//! The running state of the broker service, read without administrator rights.

use super::{programs, service_name, Os};
use std::process::Command;

/// The state from `sc.exe query` output: `Some(true)` for `RUNNING`, `Some(false)` for another
/// state, `None` when the output has no `STATE` line.
pub fn parse_sc_state(output: &str) -> Option<bool> {
    output.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == "STATE").then(|| value.split_whitespace().nth(1) == Some("RUNNING"))
    })
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
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether the broker service is running according to the service manager: `None` when that is
/// not known (not installed, the manager cannot be asked).
pub fn service_running(os: Os) -> Option<bool> {
    let name = service_name(os);
    match os {
        Os::Windows => parse_sc_state(&output_of("sc.exe", &["query", name])?),
        Os::Linux => parse_systemctl_active(&output_of("systemctl", &["is-active", name])?),
        Os::MacOs => {
            let target = format!("system/{name}");
            parse_launchctl_state(&output_of("launchctl", &["print", &target])?)
        }
    }
}

/// The command that restarts the broker service, for an elevated shell.
pub fn restart_command(os: Os) -> String {
    let name = service_name(os);
    match os {
        Os::Windows => format!("Restart-Service {name}"),
        Os::Linux => format!("sudo systemctl restart {name}"),
        Os::MacOs => format!("sudo launchctl kickstart -k system/{name}"),
    }
}
