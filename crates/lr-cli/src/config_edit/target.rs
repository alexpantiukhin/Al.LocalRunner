use crate::paths::{service_root, Layout};
use std::path::PathBuf;
use std::str::FromStr;

/// A configuration file a command edits, named by the role that reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `policy.toml` of the broker.
    Broker,
    /// `hub-policy.toml` of the hub.
    Hub,
    /// `runner.toml` of the runner of this name.
    Runner(String),
    /// `logging.toml`.
    Logging,
}

impl FromStr for Target {
    type Err = String;

    fn from_str(text: &str) -> Result<Target, String> {
        match text {
            "broker" => Ok(Target::Broker),
            "hub" => Ok(Target::Hub),
            "logging" => Ok(Target::Logging),
            other => match other.strip_prefix("runner:") {
                Some(name) if !name.is_empty() => Ok(Target::Runner(name.to_string())),
                _ => Err(format!(
                    "unknown target `{text}`: use broker, hub, runner:<name> or logging"
                )),
            },
        }
    }
}

/// The file the role really reads: the broker's service file when the broker is the OS
/// service, a runner's own copy when it exists, else the template.
pub fn resolve(layout: &Layout, target: &Target) -> Result<PathBuf, String> {
    resolve_with(
        layout,
        target,
        crate::os_service::service_installed(crate::os_service::Os::current()),
    )
}

/// [`resolve`] with the answer to "does the broker run as the OS service" given by the caller.
pub fn resolve_with(
    layout: &Layout,
    target: &Target,
    service_installed: bool,
) -> Result<PathBuf, String> {
    Ok(match target {
        Target::Broker if layout.broker_is_service_with(service_installed) => {
            service_root().join("policy.toml")
        }
        Target::Broker => layout.config_file("policy.toml"),
        Target::Hub => layout.config_file("hub-policy.toml"),
        Target::Logging => layout.config_file("logging.toml"),
        Target::Runner(name) => crate::build::runner_config_path(layout, name, None)
            .unwrap_or_else(|| layout.config_file("runner.toml")),
    })
}
