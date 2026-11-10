//! The commands of spec 11.1 implemented by this build and their dispatch.

pub mod admin_cmd;
pub mod args;
pub mod jobs;
pub mod join_cmd;
pub mod roles_cmd;
pub mod service_cmd;
pub mod setup;
pub mod view;

pub use args::*;

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
    /// Create the state directory and configuration templates of the chosen roles.
    Init {
        /// Roles to prepare.
        #[arg(long, value_enum, default_value_t = RoleArg::All)]
        role: RoleArg,
    },
    /// Install this binary for the current user, put it on PATH and run `init`.
    Install {
        /// Target directory (default: the user's program directory).
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Roles `init` prepares.
        #[arg(long, value_enum, default_value_t = RoleArg::All)]
        role: RoleArg,
        /// Allow installing as root or administrator.
        #[arg(long)]
        allow_elevated: bool,
        /// Print what would be done and change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Stop runners, remove autostart, the broker service and the binary; keys and history stay.
    Uninstall {
        /// Also delete the state directory (asks for confirmation).
        #[arg(long)]
        purge: bool,
        /// Confirm `--purge` without asking.
        #[arg(long)]
        yes: bool,
        /// Directory the binary was installed to.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Print what would be done and change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Enable or disable starting roles with the OS (`broker` = the broker service).
    Autostart {
        /// `on` or `off`.
        #[arg(value_enum)]
        switch: Switch,
        /// The role; the roles `init` created when omitted.
        #[arg(value_enum)]
        role: Option<RoleArg>,
        /// One runner of this installation (with the role `runner`).
        runner: Option<String>,
        /// Print what would be done and change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Check this node and print what is wrong.
    Doctor {
        /// Print JSON.
        #[arg(long)]
        json: bool,
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
        Command::Hub { action } => match action {
            HubAction::Run { background } => roles_cmd::hub_run(&layout, background).await,
            HubAction::Join {
                address,
                code,
                code_file,
                name,
            } => match join_cmd::join_code(code, code_file) {
                Ok(code) => join_cmd::hub_join(&layout, &address, &code, name).await,
                Err(e) => Err(e),
            },
        },
        Command::Broker { action } => match action {
            BrokerAction::Run {
                background,
                service,
            } => roles_cmd::broker_run(&layout, background, service).await,
            BrokerAction::InstallService { dry_run } => {
                service_cmd::install_service(&layout, dry_run)
            }
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
                background,
            } => roles_cmd::runner_run(&layout, name, count, config, background).await,
            RunnerAction::Join {
                address,
                code,
                code_file,
                name,
                count,
                config,
                platforms,
                tags,
            } => {
                let join = join_cmd::join_code(code, code_file).map(|code| join_cmd::RunnerJoin {
                    address,
                    code,
                    name,
                    count,
                    config,
                    platforms,
                    tags,
                });
                match join {
                    Ok(join) => join_cmd::runner_join(&layout, join).await,
                    Err(e) => Err(e),
                }
            }
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
        Command::Init { role } => setup::init(&layout, role),
        Command::Install {
            dir,
            role,
            allow_elevated,
            dry_run,
        } => setup::install(&layout, dir, role, allow_elevated, dry_run),
        Command::Uninstall {
            purge,
            yes,
            dir,
            dry_run,
        } => setup::uninstall(&layout, purge, yes, dir, dry_run).await,
        Command::Autostart {
            switch,
            role,
            runner,
            dry_run,
        } => service_cmd::autostart(&layout, switch, role, runner, dry_run),
        Command::Doctor { json } => crate::doctor::run(&layout, json).await,
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}
