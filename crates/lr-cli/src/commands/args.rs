//! Arguments of the grouped commands (spec 11.1).

use clap::{Subcommand, ValueEnum};
use std::path::PathBuf;

/// `policy ...`.
#[derive(Subcommand, Debug)]
pub enum PolicyAction {
    /// Check `policy.toml` without applying it.
    Check {
        /// The file; `policy.toml` of the configuration directory when omitted.
        file: Option<PathBuf>,
    },
    /// Check and apply `policy.toml` (local channel only).
    Reload,
}

/// `logging ...`.
#[derive(Subcommand, Debug)]
pub enum LoggingAction {
    /// Reread `logging.toml` of the broker.
    Reload,
}

/// `hub ...`.
#[derive(Subcommand, Debug)]
pub enum HubAction {
    /// Run the hub in the foreground.
    Run {
        /// Started by the OS at logon: hide or create the console.
        #[arg(long, hide = true)]
        background: bool,
    },
    /// Join a broker as a client with a one-time code.
    Join {
        /// `host:port` printed by `broker add`.
        address: String,
        /// The one-time code.
        code: String,
        /// Name of the broker in this hub's list and `hub-policy.toml` (default: the address).
        #[arg(long)]
        name: Option<String>,
    },
}

/// `broker ...`.
#[derive(Subcommand, Debug)]
pub enum BrokerAction {
    /// Run the broker in the foreground.
    Run {
        /// Started by the OS at logon: hide or create the console.
        #[arg(long, hide = true)]
        background: bool,
        /// Run under the OS service manager (`broker install-service` sets it).
        #[arg(long, hide = true)]
        service: bool,
    },
    /// Install the broker as an OS service under its own unprivileged account (administrator).
    InstallService {
        /// Print what would be done and change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Print a join code.
    Add {
        /// Client name.
        #[arg(long)]
        name: Option<String>,
        /// Client label.
        #[arg(long)]
        label: Option<String>,
        /// A code for N runners.
        #[arg(long)]
        runners: Option<u32>,
        /// Address to print.
        #[arg(long)]
        address: Option<String>,
    },
}

/// `runner ...`.
#[derive(Subcommand, Debug)]
pub enum RunnerAction {
    /// Run runners in the foreground.
    Run {
        /// Runner name.
        #[arg(long)]
        name: Option<String>,
        /// Number of runners `name-1` … `name-N`.
        #[arg(long, default_value_t = 1)]
        count: u32,
        /// `runner.toml` to use.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Started by the OS at logon: hide or create the console.
        #[arg(long, hide = true)]
        background: bool,
    },
    /// Join a broker's pool with a one-time code: one key, directory and record per runner.
    Join {
        /// `host:port` printed by `broker add`.
        address: String,
        /// The one-time code.
        code: String,
        /// Runner name.
        #[arg(long)]
        name: Option<String>,
        /// Number of runners `name-1` … `name-N`.
        #[arg(long, default_value_t = 1)]
        count: u32,
        /// `runner.toml` template to use.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Platforms, replacing those of `runner.toml`.
        #[arg(long, value_delimiter = ',')]
        platforms: Vec<String>,
        /// Tags, replacing those of `runner.toml`.
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
    },
    /// Runners of the pool (on the broker).
    List,
    /// Remove a runner and revoke its key (on the broker).
    Remove {
        /// Runner name.
        name: String,
    },
}

/// `client ...`.
#[derive(Subcommand, Debug)]
pub enum ClientAction {
    /// Clients of the broker.
    List,
    /// Remove a client and revoke its key.
    Remove {
        /// Client name.
        name: String,
    },
}

/// A role for `init`, `install` and `autostart`.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleArg {
    /// The hub.
    Hub,
    /// The broker.
    Broker,
    /// Runners.
    Runner,
    /// All three.
    All,
}

/// `on` or `off`.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    /// Enable.
    On,
    /// Disable.
    Off,
}
