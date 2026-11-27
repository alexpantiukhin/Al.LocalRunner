//! Arguments of the grouped commands (spec 11.1).

use clap::{Args, Subcommand, ValueEnum};
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

/// Parses `--arg`: `name=value`, split at the first `=`; the name must not be empty.
/// A `--zones` value: zone names separated by commas, such as `local,shared`.
pub fn parse_zones(text: &str) -> Result<std::collections::BTreeSet<lr_core::Zone>, String> {
    lr_core::Zone::parse_list(text)
}

/// A `--max-zone` value: one zone name.
pub fn parse_zone(text: &str) -> Result<lr_core::Zone, String> {
    text.trim().parse()
}

pub fn parse_arg(text: &str) -> Result<(String, String), String> {
    match text.split_once('=') {
        Some((name, value)) if !name.is_empty() => Ok((name.to_string(), value.to_string())),
        _ => Err(format!("`{text}` is not NAME=VALUE")),
    }
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
    /// Allow a secret for a client label in `[[secrets.allow]]`, then reload the broker.
    AllowSecret {
        /// The secret name.
        name: String,
        /// The client label (the label of its `[[local_user]]` or client key).
        #[arg(long, allow_hyphen_values = true)]
        label: String,
        #[command(flatten)]
        flags: RecordFlags,
    },
    /// Take a secret from a client label in `[[secrets.allow]]`.
    RemoveSecret {
        /// The secret name.
        name: String,
        /// The client label.
        #[arg(long, allow_hyphen_values = true)]
        label: String,
        #[command(flatten)]
        flags: RecordFlags,
    },
    /// Allow a service for a client label in `[[services.allow]]`.
    AllowService {
        /// The service name.
        name: String,
        /// The client label.
        #[arg(long, allow_hyphen_values = true)]
        label: String,
        #[command(flatten)]
        flags: RecordFlags,
    },
    /// Take a service from a client label in `[[services.allow]]`.
    RemoveService {
        /// The service name.
        name: String,
        /// The client label.
        #[arg(long, allow_hyphen_values = true)]
        label: String,
        #[command(flatten)]
        flags: RecordFlags,
    },
    /// Allow an OS user on the local channel with a `[[local_user]]` entry.
    AddLocalUser {
        /// The OS account name.
        #[arg(value_name = "OS_USER")]
        name: String,
        /// The client label of that user; empty when omitted.
        #[arg(long, allow_hyphen_values = true)]
        label: Option<String>,
        #[command(flatten)]
        flags: RecordFlags,
    },
    /// Remove the `[[local_user]]` entry of an OS user.
    RemoveLocalUser {
        /// The OS account name.
        #[arg(value_name = "OS_USER")]
        name: String,
        #[command(flatten)]
        flags: RecordFlags,
    },
    /// Add a `[[shared]]` block: client labels and runner names that reach one shared disk.
    AddShared {
        /// The members (at least two different names).
        #[arg(required = true, value_name = "MEMBER")]
        members: Vec<String>,
        #[command(flatten)]
        flags: RecordFlags,
    },
    /// Remove the `[[shared]]` block with exactly these members, in any order.
    RemoveShared {
        /// The members.
        #[arg(required = true, value_name = "MEMBER")]
        members: Vec<String>,
        #[command(flatten)]
        flags: RecordFlags,
    },
}

/// The flags of every `policy` command that changes a record.
#[derive(Args, Debug, Clone, Copy)]
pub struct RecordFlags {
    /// Show the change and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Restart the broker when the list applies only at its next start (connections drop).
    #[arg(long)]
    pub restart: bool,
}

/// Parses the target of `config`: `broker`, `hub`, `runner:<name>` or `logging`.
pub fn parse_target(text: &str) -> Result<crate::config_edit::Target, String> {
    text.parse()
}

/// `config ...`.
#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Set one key in the file the role reads, show the diff, write it with a backup and reload
    /// the role.
    Set {
        /// `broker`, `hub`, `runner:<name>` or `logging`.
        #[arg(value_parser = parse_target)]
        target: crate::config_edit::Target,
        /// The dotted key, such as `queue.strategy`.
        key: String,
        /// The value as TOML (`5`, `true`, `["a", "b"]`); other text is a string.
        #[arg(allow_hyphen_values = true)]
        value: String,
        /// Store the value as a string.
        #[arg(long)]
        string: bool,
        /// Show the change and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Restart the role when the key applies only at its next start (connections drop).
        #[arg(long)]
        restart: bool,
    },
    /// Remove one key from the file the role reads; the role uses its default.
    Unset {
        /// `broker`, `hub`, `runner:<name>` or `logging`.
        #[arg(value_parser = parse_target)]
        target: crate::config_edit::Target,
        /// The dotted key.
        key: String,
        /// Show the change and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Restart the role when the key applies only at its next start (connections drop).
        #[arg(long)]
        restart: bool,
    },
    /// Print the value of one key.
    Get {
        /// `broker`, `hub`, `runner:<name>` or `logging`.
        #[arg(value_parser = parse_target)]
        target: crate::config_edit::Target,
        /// The dotted key.
        key: String,
    },
    /// Print the path and the text of the file the role reads.
    Show {
        /// `broker`, `hub`, `runner:<name>` or `logging`.
        #[arg(value_parser = parse_target)]
        target: crate::config_edit::Target,
    },
}

/// `secret ...`.
#[derive(Subcommand, Debug)]
pub enum SecretAction {
    /// Store a secret of a runner: the value is read from standard input or a file (never from
    /// the command line) into a private file, and `secret.NAME.from` points at it.
    Set {
        /// The runner whose `runner.toml` gets the secret.
        runner: String,
        /// The secret name (letters, digits, `-` and `_`).
        name: String,
        /// Read the value from this file instead of standard input.
        #[arg(long)]
        value_file: Option<PathBuf>,
        /// Show the change and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Restart the runner when the change applies only at its next start.
        #[arg(long)]
        restart: bool,
    },
    /// List the secrets of a runner (all runners without a name) with their sources and whether
    /// they give a value now; values are never printed.
    List {
        /// The runner; every runner of the installation when omitted.
        runner: Option<String>,
    },
    /// Remove a secret from a runner and delete the value file `secret set` made for it.
    Remove {
        /// The runner.
        runner: String,
        /// The secret name.
        name: String,
        /// Show the change and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Restart the runner when the change applies only at its next start.
        #[arg(long)]
        restart: bool,
    },
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
        /// Run under the OS service manager (`autostart on runner <name> --service` sets it).
        #[arg(long, hide = true, requires = "name")]
        service: bool,
        /// The OS user the installation belongs to, when the service account is another one.
        #[arg(long, hide = true, requires = "service")]
        owner: Option<String>,
        /// Directory where the jobs of this runner work (`workspace/`, `cache/`, `tmp/`,
        /// `services/`); the key, the database and the logs stay in the state directory. Use it
        /// when the jobs' account cannot reach the state directory's parents. Needs one runner.
        #[arg(long)]
        job_root: Option<PathBuf>,
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
    /// The outbox of a runner: what it still has to deliver to the broker.
    Outbox {
        /// What to do with it.
        #[command(subcommand)]
        action: OutboxAction,
    },
    /// The named caches of a runner.
    Cache {
        /// What to do with them.
        #[command(subcommand)]
        action: CacheAction,
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

/// `runner cache ...`.
#[derive(Subcommand, Debug)]
pub enum CacheAction {
    /// Remove the named caches that no running job uses, to free the disk of a runner; without
    /// `--older-than` all of them.
    Clean {
        /// Runner name; default: the only runner of this installation.
        name: Option<String>,
        /// Only the caches unused for more than this many days.
        #[arg(long, value_name = "DAYS")]
        older_than: Option<u64>,
        /// Show what would be removed, remove nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

/// `runner outbox ...`.
#[derive(Subcommand, Debug)]
pub enum OutboxAction {
    /// Close the pending outbox entries of a job the broker has cancelled or lost; refused
    /// while the runner still runs the job.
    Drop {
        /// Runner name; default: the only runner of this installation.
        #[arg(long)]
        runner: Option<String>,
        /// The job whose entries are closed.
        job: String,
    },
}

/// `profile ...`.
#[derive(Subcommand, Debug)]
pub enum ProfileAction {
    /// Check the profiles file of this project: print a summary of each profile, warn about
    /// ignored top-level sections, then check the secrets: whether the policy lets the client
    /// request them, whether a runner of this machine has a source for each, and the variable
    /// each one becomes in the job.
    Check {
        /// Profile name; without one, every profile.
        profile: Option<String>,
        /// The client label the policy compares with: the `label` of the `[[local_user]]`
        /// entry, not the `--as` of a run. Default: the entry of this OS user.
        #[arg(long)]
        label: Option<String>,
        /// The profiles file to check. Default: `.localrunner/profiles.toml`.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Print one JSON document instead of text.
        #[arg(long)]
        json: bool,
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
    /// Show what starts with the OS and how; changes nothing.
    Status,
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
    /// With running jobs: stop waiting for them after this long (`90s`, `15m`, `2h`); nothing is
    /// replaced then.
    #[arg(long, value_parser = crate::commands::upgrade_cmd::parse_duration)]
    pub wait_timeout: Option<std::time::Duration>,
    /// The release to install, `X.Y.Z` (default: the latest release that is not a pre-release);
    /// only when run from the installed binary.
    #[arg(long = "version", value_name = "X.Y.Z")]
    pub version: Option<String>,
    /// Install from local files: the archive (or a folder holding it) with `SHA256SUMS` and
    /// `SHA256SUMS.sig` next to it; nothing is downloaded.
    #[arg(long, value_name = "ARCHIVE")]
    pub offline: Option<PathBuf>,
}
