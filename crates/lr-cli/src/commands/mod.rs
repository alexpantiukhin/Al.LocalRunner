//! The commands of spec 11.1 implemented by this build and their dispatch.

pub mod admin_cmd;
pub mod jobs;
pub mod roles_cmd;
pub mod view;

use crate::paths::Layout;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// `localrunner`: one binary for every role (spec 11.1).
#[derive(Parser, Debug)]
#[command(name = "localrunner")]
pub struct Cli {
    /// State directory (default: %LOCALAPPDATA%\Al.LocalRunner or ~/.local/share/al-localrunner).
    #[arg(long, global = true)]
    pub state_dir: Option<PathBuf>,
    /// Configuration directory (default: the state directory; LOCALRUNNER_CONFIG_DIR).
    #[arg(long, global = true)]
    pub config_dir: Option<PathBuf>,
    /// The command.
    #[command(subcommand)]
    pub command: Command,
}

/// Fields of an order shared by `submit` and `run`.
#[derive(Args, Debug, Clone)]
pub struct OrderArgs {
    /// Profile name; the only profile when omitted.
    pub profile: Option<String>,
    /// Submitter label (required).
    #[arg(long = "as", value_name = "LABEL", required = true)]
    pub label: String,
    /// Free note stored with the run.
    #[arg(long)]
    pub note: Option<String>,
    /// One platform; makes it a commit order.
    #[arg(long)]
    pub platform: Option<String>,
    /// Lowers the profile timeout, in seconds.
    #[arg(long)]
    pub timeout: Option<u64>,
    /// The order outlives the submitting process.
    #[arg(long)]
    pub detach: bool,
}

/// `--source` of `status`.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceArg {
    /// Terminal or agent session.
    Session,
    /// GitHub Actions.
    Github,
}

/// The commands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Place an order and print its JobId.
    Submit {
        /// Order fields.
        #[command(flatten)]
        order: OrderArgs,
        /// Stream the output and end with the job's exit code.
        #[arg(long)]
        wait: bool,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// `submit --wait`: place an order, stream its output, return its exit code.
    Run {
        /// Order fields.
        #[command(flatten)]
        order: OrderArgs,
    },
    /// Watch a job; interrupting cancels nothing.
    Wait {
        /// The job.
        job: String,
    },
    /// Own jobs (label `--as` and OS user).
    Jobs {
        /// Submitter label.
        #[arg(long = "as", value_name = "LABEL", required = true)]
        label: String,
        /// Own jobs (the only mode).
        #[arg(long)]
        mine: bool,
        /// Only queued and running jobs.
        #[arg(long)]
        active: bool,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Cancel a job, or `--mine` for all own active jobs.
    Cancel {
        /// The job.
        job: Option<String>,
        /// Submitter label of the canceller.
        #[arg(long = "as", value_name = "LABEL", required = true)]
        label: String,
        /// All own active jobs.
        #[arg(long)]
        mine: bool,
        /// Cancel a job of another label.
        #[arg(long)]
        force: bool,
        /// Reason written to the log.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Queue and running jobs with steps and wait reasons.
    Status {
        /// Only this project.
        #[arg(long)]
        project: Option<String>,
        /// Only this source.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
        /// Only this label.
        #[arg(long = "as", value_name = "LABEL")]
        label: Option<String>,
        /// Only this broker.
        #[arg(long)]
        broker: Option<String>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Known brokers: zone, availability, orders in the outbox.
    Brokers {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Live dashboard on loopback.
    Dashboard,
    /// Stop runners: finish the current job (default) or cancel it.
    Down {
        /// Runner names; all runners of this installation when omitted.
        names: Vec<String>,
        /// Finish the current job first (default).
        #[arg(long, conflicts_with = "cancel")]
        drain: bool,
        /// Cancel the current job.
        #[arg(long)]
        cancel: bool,
    },
    /// Broker policy.
    Policy {
        /// What to do.
        #[command(subcommand)]
        action: PolicyAction,
    },
    /// Diagnostics configuration.
    Logging {
        /// What to do.
        #[command(subcommand)]
        action: LoggingAction,
    },
    /// The hub role.
    Hub {
        /// What to do.
        #[command(subcommand)]
        action: HubAction,
    },
    /// The broker role.
    Broker {
        /// What to do.
        #[command(subcommand)]
        action: BrokerAction,
    },
    /// Runners.
    Runner {
        /// What to do.
        #[command(subcommand)]
        action: RunnerAction,
    },
    /// Clients (hubs) of the broker.
    Client {
        /// What to do.
        #[command(subcommand)]
        action: ClientAction,
    },
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
    /// Reread `logging.toml` of the broker.
    Reload,
}

/// `hub ...`.
#[derive(Subcommand, Debug)]
pub enum HubAction {
    /// Run the hub in the foreground.
    Run,
}

/// `broker ...`.
#[derive(Subcommand, Debug)]
pub enum BrokerAction {
    /// Run the broker in the foreground.
    Run,
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

/// The `--version` text: binary and protocol version.
pub fn version_text() -> String {
    let p = lr_proto::PROTOCOL_VERSION;
    format!(
        "{} (protocol {}.{})",
        env!("CARGO_PKG_VERSION"),
        p.major,
        p.minor
    )
}

/// Runs `cli`; the result is the process exit code.
pub async fn run(cli: Cli) -> i32 {
    let layout = match Layout::resolve(cli.state_dir, cli.config_dir) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let result = match cli.command {
        Command::Submit { order, wait, json } => jobs::submit(&layout, order, wait, json).await,
        Command::Run { order } => jobs::submit(&layout, order, true, false).await,
        Command::Wait { job } => jobs::wait(&layout, &job).await,
        Command::Jobs {
            label,
            active,
            json,
            ..
        } => jobs::jobs(&layout, &label, active, json).await,
        Command::Cancel {
            job,
            label,
            mine,
            force,
            reason,
        } => jobs::cancel(&layout, job, &label, mine, force, reason).await,
        Command::Status {
            project,
            source,
            label,
            broker,
            json,
        } => view::status(&layout, project, source, label, broker, json).await,
        Command::Brokers { json } => view::brokers(&layout, json).await,
        Command::Dashboard => crate::dashboard::run(&layout).await,
        Command::Down { names, cancel, .. } => roles_cmd::down(&layout, names, cancel).await,
        Command::Policy { action } => match action {
            PolicyAction::Check { file } => admin_cmd::policy_check(&layout, file).await,
            PolicyAction::Reload => {
                admin_cmd::simple(&layout, lr_proto::AdminRequest::PolicyReload).await
            }
        },
        Command::Logging {
            action: LoggingAction::Reload,
        } => admin_cmd::simple(&layout, lr_proto::AdminRequest::LoggingReload).await,
        Command::Hub {
            action: HubAction::Run,
        } => roles_cmd::hub_run(&layout).await,
        Command::Broker { action } => match action {
            BrokerAction::Run => roles_cmd::broker_run(&layout).await,
            BrokerAction::Add {
                name,
                label,
                runners,
                address,
            } => {
                let req = lr_proto::AdminRequest::BrokerAdd {
                    name,
                    label,
                    runners,
                    address,
                };
                admin_cmd::simple(&layout, req).await
            }
        },
        Command::Runner { action } => match action {
            RunnerAction::Run {
                name,
                count,
                config,
            } => roles_cmd::runner_run(&layout, name, count, config).await,
            RunnerAction::List => {
                admin_cmd::simple(&layout, lr_proto::AdminRequest::RunnerList).await
            }
            RunnerAction::Remove { name } => {
                admin_cmd::simple(&layout, lr_proto::AdminRequest::RunnerRemove { name }).await
            }
        },
        Command::Client { action } => match action {
            ClientAction::List => {
                admin_cmd::simple(&layout, lr_proto::AdminRequest::ClientList).await
            }
            ClientAction::Remove { name } => {
                admin_cmd::simple(&layout, lr_proto::AdminRequest::ClientRemove { name }).await
            }
        },
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}
