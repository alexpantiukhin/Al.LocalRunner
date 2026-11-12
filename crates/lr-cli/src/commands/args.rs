//! Arguments of the grouped commands (spec 11.1).

use clap::{Subcommand, ValueEnum};
use std::path::PathBuf;

/// Parses `--mem`: megabytes (`6144`) or a number with `m` or `g` (`512m`, `6g`, `g` = 1024 MB).
pub fn parse_mem_mb(text: &str) -> Result<u64, String> {
    let bad = || format!("`{text}` is not a memory size: give MB (6144) or a unit (6g, 512m)");
    let (digits, factor) = match text.as_bytes().last() {
        Some(b'g' | b'G') => (&text[..text.len() - 1], 1024),
        Some(b'm' | b'M') => (&text[..text.len() - 1], 1),
        _ => (text, 1),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let mb = digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(factor))
        .ok_or_else(bad)?;
    if mb == 0 {
        return Err(bad());
    }
    Ok(mb)
}

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
    /// Reread `logging.toml` in every running role (broker, hub, runners).
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
        /// The one-time code; better `--code-file`, `LOCALRUNNER_JOIN_CODE` or standard input,
        /// since other local users see the command line.
        code: Option<String>,
        /// File with the code; `-` reads it from standard input.
        #[arg(long)]
        code_file: Option<PathBuf>,
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
    /// Replace the broker service's own copy of the binary with this one and restart the
    /// service (administrator).
    UpdateService {
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
        /// Write the code to this new file instead of printing it: the joining user reads it
        /// with `runner join --code-file`.
        #[arg(long)]
        code_file: Option<PathBuf>,
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
        /// The one-time code; better `--code-file`, `LOCALRUNNER_JOIN_CODE` or standard input,
        /// since other local users see the command line.
        code: Option<String>,
        /// File with the code; `-` reads it from standard input.
        #[arg(long)]
        code_file: Option<PathBuf>,
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
    /// Show the `runner.toml` a runner reads, or with `--diff` how its own copy differs from
    /// the template.
    Config {
        /// Runner name.
        name: String,
        /// Compare the own copy with the template.
        #[arg(long)]
        diff: bool,
    },
    /// Runners of the pool (on the broker).
    List,
    /// Remove a runner and revoke its key (on the broker; asks for confirmation).
    Remove {
        /// Runner name.
        name: String,
        /// Confirm without asking.
        #[arg(long)]
        yes: bool,
    },
}

/// `profile ...`.
#[derive(Subcommand, Debug)]
pub enum ProfileAction {
    /// Check the secrets of the profiles of this project: whether the policy lets the client
    /// request them, whether a runner of this machine has a source for each, and the variable
    /// each one becomes in the job.
    Check {
        /// Profile name; without one, every profile that asks for a secret.
        profile: Option<String>,
        /// The client label the policy compares with: the `label` of the `[[local_user]]`
        /// entry, not the `--as` of a run. Default: the entry of this OS user.
        #[arg(long)]
        label: Option<String>,
    },
}

/// `client ...`.
#[derive(Subcommand, Debug)]
pub enum ClientAction {
    /// Clients of the broker.
    List,
    /// Remove a client and revoke its key (asks for confirmation).
    Remove {
        /// Client name.
        name: String,
        /// Confirm without asking.
        #[arg(long)]
        yes: bool,
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

/// `upgrade [--dir] [--yes] [--allow-elevated] [--dry-run]`.
#[derive(clap::Args, Debug, Clone)]
pub struct UpgradeArgs {
    /// Directory the binary was installed to (default: the user's program directory).
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Confirm without asking; with running jobs it waits for them.
    #[arg(long)]
    pub yes: bool,
    /// With running jobs: cancel them instead of waiting for them to finish.
    #[arg(long)]
    pub cancel: bool,
    /// Allow upgrading as root or administrator.
    #[arg(long)]
    pub allow_elevated: bool,
    /// Print what would be done and change nothing.
    #[arg(long)]
    pub dry_run: bool,
}
