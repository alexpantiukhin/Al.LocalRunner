//! The commands of spec 11.1 implemented by this build and their dispatch.

pub mod admin_cmd;
pub mod args;
pub mod config_cmd;
pub mod confirm;
pub mod down_cmd;
pub mod history_cmd;
pub mod jobs;
pub mod join_cmd;
pub mod log_cmd;
pub mod logging_cmd;
pub mod policy_edit_cmd;
pub mod profile_cmd;
pub mod profile_summary;
pub mod read_args;
pub mod read_cmd;
pub mod reload_cmd;
pub mod rights;
pub mod roles_cmd;
pub mod secret_cmd;
pub mod variant_check;
pub mod runner_config_cmd;
pub mod runner_outbox_cmd;
pub mod service_cmd;
pub mod setup;
pub mod snapshot_cmd;
pub mod timefmt;
pub mod up_cmd;
pub mod upgrade_cmd;
pub mod upgrade_fetch;
pub mod upgrade_jobs;
pub mod upgrade_pool;
pub mod view;

pub use args::*;
pub use read_args::*;

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
    /// Marks the elevated re-run of a command that needs administrator rights; on Windows the
    /// value names the output channel. Hidden, accepted only before the subcommand.
    #[arg(
        long,
        hide = true,
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "",
        value_name = "CHANNEL"
    )]
    pub elevated_child: Option<String>,
    /// In the elevated re-run: the SHA-256 of the file text the parent showed the diff for; the
    /// command writes nothing when the file no longer has it. Hidden.
    #[arg(long, hide = true, require_equals = true, value_name = "SHA256")]
    pub expect_before: Option<String>,
    /// Never ask for administrator rights: a command that needs them fails and prints the
    /// command to run with them.
    #[arg(long, global = true)]
    pub no_elevate: bool,
    /// The command.
    #[command(subcommand)]
    pub command: Command,
}

/// Fields of an order shared by `submit` and `run`.
#[derive(Args, Debug, Clone)]
pub struct OrderArgs {
    /// Profile name; the only profile when omitted.
    pub profile: Option<String>,
    /// Submitter label: required, except inside GitHub Actions (`github:<workflow>/<job>`).
    #[arg(long = "as", value_name = "LABEL")]
    pub label: Option<String>,
    /// Free note stored with the run.
    #[arg(long)]
    pub note: Option<String>,
    /// One platform: the profile is resolved for it.
    #[arg(long)]
    pub platform: Option<String>,
    /// Lowers the profile timeout, in seconds.
    #[arg(long)]
    pub timeout: Option<u64>,
    /// Memory for this run instead of the profile value: MB (`6144`) or with a unit (`6g`, `512m`).
    #[arg(long, value_name = "SIZE", value_parser = parse_mem_mb)]
    pub mem: Option<u64>,
    /// CPU cores for this run instead of the profile value.
    #[arg(long, value_name = "CORES", value_parser = clap::value_parser!(u32).range(1..))]
    pub cpu: Option<u32>,
    /// A profile argument, `name=value`; repeat the flag for several values.
    #[arg(long = "arg", value_name = "NAME=VALUE", value_parser = parse_arg)]
    pub args: Vec<(String, String)>,
    /// Zones a runner may be in, nearest first: local, shared, lan, remote; a list such as
    /// `local,shared`. Without it every zone the profile has steps for is allowed.
    #[arg(long, value_name = "ZONES", value_parser = args::parse_zones, conflicts_with = "max_zone")]
    pub zones: Option<std::collections::BTreeSet<lr_core::Zone>>,
    /// Shorthand for `--zones` from local up to this zone.
    #[arg(long = "max-zone", value_name = "ZONE", value_parser = args::parse_zone)]
    pub max_zone: Option<lr_core::Zone>,
    /// Make the order outlive the sending process; `submit` without `--wait` already does, so it
    /// only matters for `submit --wait` and `run`, where the order is otherwise cancelled when
    /// the sending process exits.
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
        /// The order is cancelled when the sending process exits (the behavior before `submit`
        /// detached by default).
        #[arg(long, conflicts_with = "detach")]
        no_detach: bool,
        /// Do not print the follow, result and cancel hints to stderr.
        #[arg(long)]
        quiet: bool,
    },
    /// `submit --wait`: place an order, stream its output, return its exit code.
    Run {
        /// Order fields.
        #[command(flatten)]
        order: OrderArgs,
    },
    /// Watch jobs; interrupting cancels nothing. One job prints the last 50 lines of its log
    /// (`--full`: all of it) and ends with its exit code; several jobs (or `--as` alone) print one result line per job.
    Wait {
        /// The jobs; with none, the active jobs of `--as` (else its last job).
        #[arg(required_unless_present = "label")]
        jobs: Vec<String>,
        /// Submitter label: with ids, checks that the jobs are its own; alone, selects its jobs.
        #[arg(long = "as", value_name = "LABEL")]
        label: Option<String>,
        /// Print JSON: the output goes to stderr, stdout gets the final state (one line per job).
        #[arg(long)]
        json: bool,
        /// One job: print the whole log, not its last 50 lines and a hint.
        #[arg(long)]
        full: bool,
        /// Only this broker: refused at once when this hub does not know it or cannot reach it.
        /// A moved order is still followed to the broker that holds it.
        #[arg(long)]
        broker: Option<String>,
    },
    /// Own jobs (label `--as` and OS user).
    Jobs {
        /// Submitter label; without it, the label of the last handle in `temp/runner/`.
        #[arg(long = "as", value_name = "LABEL")]
        label: Option<String>,
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
        #[arg(long = "as", value_name = "LABEL")]
        label: Option<String>,
        /// All own active jobs.
        #[arg(long)]
        mine: bool,
        /// Cancel a job of another label.
        #[arg(long)]
        force: bool,
        /// Reason written to the log.
        #[arg(long)]
        reason: Option<String>,
        /// Confirm `--mine` without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Queue and running jobs with steps and wait reasons.
    Status {
        /// Show this one job: why it waits or what it does, with its record.
        #[arg(value_name = "JOB", conflicts_with = "html")]
        job: Option<String>,
        /// The client key whose run to show, when several clients use the same job id.
        #[arg(long, requires = "job")]
        client: Option<String>,
        /// Only this project; not with `JOB`.
        #[arg(long, conflicts_with = "job")]
        project: Option<String>,
        /// Only this source; not with `JOB`.
        #[arg(long, value_enum, conflicts_with = "job")]
        source: Option<SourceArg>,
        /// Only this label; with `JOB` it is accepted and ignored: the job id is enough.
        #[arg(long = "as", value_name = "LABEL")]
        label: Option<String>,
        /// Only this broker.
        #[arg(long)]
        broker: Option<String>,
        /// Also the orders that left the queue (finished, cancelled, withdrawn, rejected) with
        /// their reason and who ended them: the last N (10 when no value) or those of a period
        /// (`30m`, `2h`, `7d`).
        #[arg(
            long,
            value_name = "N|DURATION",
            num_args = 0..=1,
            default_missing_value = "10",
            value_parser = timefmt::parse_recent,
            conflicts_with_all = ["job", "html"]
        )]
        recent: Option<timefmt::Recent>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
        /// The HTML snapshot instead of text.
        #[command(flatten)]
        html: SnapshotArgs,
    },
    /// Why an order waits, naming the policy rule that holds it.
    Explain(ExplainArgs),
    /// Runs of this client, newest first; `history show <job>` is the full record.
    History(HistoryArgs),
    /// The stored log of a job; `-f` follows a running job.
    Log(LogArgs),
    /// Statistics and pool saturation.
    Stats(StatsArgs),
    /// Runners: state, platforms, programs, pool load, tags, services, cache.
    Runners(RunnersArgs),
    /// Start the hub, the broker and the runners as separate processes, except those running.
    Up {
        /// Runner names (also `local:<name>`), `runner` (every runner), `hub`, `broker` or `all`;
        /// the roles `init` created when omitted.
        names: Vec<String>,
    },
    /// Running roles reread their settings files without a restart: runners `runner.toml`, the
    /// hub `hub-policy.toml`, the broker `policy.toml`.
    Reload {
        /// Only this role.
        #[arg(long, value_enum)]
        role: Option<RoleArg>,
        /// One runner of this installation, or `hub` or `broker`.
        name: Option<String>,
    },
    /// Known brokers: zone, availability, orders in the outbox.
    Brokers {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Live dashboard on loopback: starts it in the background when it does not run and prints
    /// the link (one line, with a new one-time token), then exits; `down` stops it.
    Dashboard {
        /// Serve in this terminal until Ctrl+C instead of in the background.
        #[arg(long)]
        foreground: bool,
        /// Started by `dashboard` itself: hide or create the console.
        #[arg(long, hide = true, requires = "foreground")]
        background: bool,
    },
    /// Stop runners, the hub and the broker: runners finish the current job (default) or
    /// cancel it.
    Down {
        /// Runner names, `runner` (every runner), `hub` or `broker`; every role of this
        /// installation when omitted.
        names: Vec<String>,
        /// Finish the current job first (default).
        #[arg(long, conflicts_with = "cancel")]
        drain: bool,
        /// Cancel the current job (asks for confirmation).
        #[arg(long)]
        cancel: bool,
        /// Confirm `--cancel` without asking.
        #[arg(long)]
        yes: bool,
        /// Return only when the stopped runners have exited (name the ones that did not); without
        /// `--cancel` first wait until the hub holds no order.
        #[arg(long)]
        wait: bool,
        /// With `--wait`: how long to wait (`90s`, `15m`, `2h`; default 60s).
        #[arg(long, requires = "wait", value_parser = crate::commands::upgrade_cmd::parse_duration)]
        wait_timeout: Option<std::time::Duration>,
    },
    /// Broker policy.
    Policy {
        /// What to do.
        #[command(subcommand)]
        action: PolicyAction,
    },
    /// Change one setting of the file a role reads: `broker`, `hub`, `runner:<name>`,
    /// `logging`.
    Config {
        /// What to do.
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Secrets of a runner: the value goes into a private file, `[secret.NAME]` of its
    /// `runner.toml` points at it.
    Secret {
        /// What to do.
        #[command(subcommand)]
        action: SecretAction,
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
    /// Run by the new binary: stop the roles, replace the installed binary with this one and start the roles again.
    Upgrade(#[command(flatten)] UpgradeArgs),
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
    /// Enable or disable starting roles with the OS: the hub and runners start at user login,
    /// `broker` is the broker service, `runner <name> --service` makes one runner an OS service.
    Autostart {
        /// `on`, `off` or `status`.
        #[arg(value_enum)]
        switch: Switch,
        /// With `status`: print JSON.
        #[arg(long)]
        json: bool,
        /// The role; the roles `init` created when omitted.
        #[arg(value_enum)]
        role: Option<RoleArg>,
        /// One runner of this installation (with the role `runner`).
        runner: Option<String>,
        /// With `runner <name>`: an OS service that starts after a reboot and survives logout,
        /// instead of the start at user login (administrator).
        #[arg(long)]
        service: bool,
        /// Print what would be done and change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Checks of the project profiles.
    Profile {
        /// What to check.
        #[command(subcommand)]
        action: ProfileAction,
    },
    /// Check this node and print what is wrong.
    Doctor {
        /// Print JSON.
        #[arg(long)]
        json: bool,
        /// Also check the policy.toml of the broker service, which needs administrator rights:
        /// asks for them (not with --json).
        #[arg(long, conflicts_with = "json")]
        elevate: bool,
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

impl Cli {
    /// Whether this process is the elevated re-run of a command (`--elevated-child`): it must
    /// not elevate again nor show the diff the parent already showed.
    pub fn is_elevated_child(&self) -> bool {
        self.elevated_child.is_some()
    }
}

async fn secret(
    layout: &Layout,
    action: SecretAction,
    elevated_child: bool,
) -> Result<i32, String> {
    use secret_cmd::{list_lines, remove_with, set_with, SetArgs, ValueSource};
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    let (elevator, roles) = (crate::config_edit::OsElevator, crate::config_edit::LiveRoles);
    match action {
        SecretAction::List { runner } => {
            for line in list_lines(layout, runner.as_deref())? {
                println!("{line}");
            }
            Ok(0)
        }
        SecretAction::Remove {
            runner,
            name,
            dry_run,
            restart,
        } => {
            remove_with(
                layout,
                &runner,
                &name,
                dry_run,
                restart,
                elevated_child,
                &elevator,
                &roles,
                &mut out,
                &mut err,
            )
            .await
        }
        SecretAction::Set {
            runner,
            name,
            value_file,
            dry_run,
            restart,
        } => {
            use std::io::IsTerminal;
            let source = match value_file {
                Some(path) => ValueSource::File(path),
                None if std::io::stdin().is_terminal() => {
                    return Err("give the value on standard input (pipe it: `... | localrunner secret set <runner> <NAME>`) or with --value-file <file>; it is never taken from the command line".into())
                }
                None => ValueSource::Stdin,
            };
            let args = SetArgs {
                runner,
                name,
                source,
                dry_run,
                restart,
            };
            set_with(
                layout,
                args,
                elevated_child,
                &elevator,
                &roles,
                &mut std::io::stdin(),
                &mut out,
                &mut err,
            )
            .await
        }
    }
}

async fn config(
    layout: &Layout,
    action: ConfigAction,
    elevated_child: bool,
    expected_before: Option<String>,
) -> Result<i32, String> {
    use config_cmd::{Edit, EditArgs};
    let print = |text: String| {
        println!("{text}");
        0
    };
    let args = match action {
        ConfigAction::Get { target, key } => {
            return config_cmd::get(layout, &target, &key).map(print)
        }
        ConfigAction::Show { target } => return config_cmd::show(layout, &target).map(print),
        ConfigAction::Set {
            target,
            key,
            value,
            string,
            dry_run,
            restart,
        } => EditArgs {
            target,
            edit: Edit::Set { key, value, string },
            dry_run,
            restart,
            expected_before,
        },
        ConfigAction::Unset {
            target,
            key,
            dry_run,
            restart,
        } => EditArgs {
            target,
            edit: Edit::Unset { key },
            dry_run,
            restart,
            expected_before,
        },
    };
    config_cmd::edit(layout, args, elevated_child).await
}

async fn policy(
    layout: &Layout,
    action: PolicyAction,
    elevated_child: bool,
    expected_before: Option<String>,
) -> Result<i32, String> {
    use policy_edit_cmd::{Record, RecordArgs};
    let (record, flags) = match action {
        PolicyAction::Check { file } => return admin_cmd::policy_check(layout, file).await,
        PolicyAction::Reload => return admin_cmd::policy_reload(layout).await,
        PolicyAction::AllowSecret { name, label, flags } => {
            (Record::AllowSecret { name, label }, flags)
        }
        PolicyAction::RemoveSecret { name, label, flags } => {
            (Record::RemoveSecret { name, label }, flags)
        }
        PolicyAction::AllowService { name, label, flags } => {
            (Record::AllowService { name, label }, flags)
        }
        PolicyAction::RemoveService { name, label, flags } => {
            (Record::RemoveService { name, label }, flags)
        }
        PolicyAction::AddLocalUser { name, label, flags } => {
            (Record::AddLocalUser { name, label }, flags)
        }
        PolicyAction::RemoveLocalUser { name, flags } => (Record::RemoveLocalUser { name }, flags),
        PolicyAction::AddShared { members, flags } => (Record::AddShared { members }, flags),
        PolicyAction::RemoveShared { members, flags } => (Record::RemoveShared { members }, flags),
    };
    let args = RecordArgs {
        record,
        dry_run: flags.dry_run,
        restart: flags.restart,
        expected_before,
    };
    policy_edit_cmd::edit(layout, args, elevated_child).await
}

/// What a command of the broker's admin channel does when the OS refused it the channel: with a
/// terminal it asks, then runs the same command as the broker's account (a broker service on
/// Unix) or with administrator rights; without one it prints the exact line to run.
fn broker_admin_retry(
    result: Result<i32, String>,
    what: &str,
    layout: &Layout,
    tail: &[String],
    no_elevate: bool,
    elevated_child: bool,
) -> Result<i32, String> {
    let need = rights::Need {
        what,
        why: "open the admin channel of the broker",
    };
    let session = rights::session(false, no_elevate, elevated_child);
    rights::retry_as_broker(result, &need, &session, layout, tail)
}

/// The arguments of `broker add` that repeat the request, for the re-run as the broker's account.
fn broker_add_flags(
    req: &lr_proto::AdminRequest,
    code_file: Option<&std::path::Path>,
) -> Vec<String> {
    let mut flags = Vec::new();
    if let lr_proto::AdminRequest::BrokerAdd {
        name,
        label,
        runners,
        address,
    } = req
    {
        let named = [("--name", name.clone()), ("--label", label.clone())]
            .into_iter()
            .chain([("--runners", runners.map(|n| n.to_string()))])
            .chain([("--address", address.clone())]);
        for (flag, value) in named {
            if let Some(v) = value {
                flags.push(flag.to_string());
                flags.push(v);
            }
        }
    }
    flags.extend(
        code_file
            .iter()
            .flat_map(|f| ["--code-file".to_string(), f.display().to_string()]),
    );
    flags
}

/// Runs `cli`; the result is the process exit code.
pub async fn run(cli: Cli) -> i32 {
    let elevated_child = cli.is_elevated_child();
    let no_elevate = cli.no_elevate;
    let expect_before = cli.expect_before.clone();
    if let Some(channel) = cli.elevated_child.as_deref().filter(|c| !c.is_empty()) {
        if let Err(e) = crate::config_edit::attach_elevated_output(channel) {
            eprintln!("error: {e}");
            return 2;
        }
    }
    let state_flag = cli.state_dir.clone();
    let layout = match Layout::resolve(cli.state_dir, cli.config_dir) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if matches!(
        cli.command,
        Command::Status { .. } | Command::Doctor { .. } | Command::Up { .. } | Command::Down { .. }
    ) {
        let env = std::env::var_os(crate::paths::STATE_DIR_ENV);
        if let Some(note) = layout.state_dir_note(state_flag.as_deref(), env.as_deref()) {
            eprintln!("{note}");
        }
    }
    let result = match cli.command {
        Command::Submit {
            order,
            wait,
            json,
            no_detach,
            quiet,
        } => jobs::submit(&layout, order, wait, json, no_detach, quiet).await,
        Command::Run { order } => jobs::submit(&layout, order, true, false, false, false).await,
        Command::Wait {
            jobs,
            label,
            json,
            full,
            broker,
        } => jobs::wait_jobs(&layout, jobs, label, json, full, broker).await,
        Command::Jobs {
            label,
            active,
            json,
            ..
        } => jobs::jobs(&layout, label, active, json).await,
        Command::Cancel {
            job,
            label,
            mine,
            force,
            reason,
            yes,
        } => jobs::cancel_cli(&layout, job, label, mine, force, reason, yes).await,
        Command::Status {
            job,
            client,
            project,
            source,
            label,
            broker,
            recent,
            json,
            html,
        } => match html.html.clone() {
            Some(path) => snapshot_cmd::write(&layout, project, source, label, path, &html).await,
            None => match job {
                Some(job) => view::status_job(&layout, job, broker, client, json).await,
                None => {
                    let filter = view::StatusArgs {
                        project,
                        source,
                        label,
                        broker,
                        recent,
                    };
                    view::status(&layout, filter, json).await
                }
            },
        },
        Command::Explain(a) => read_cmd::explain_cmd(&layout, a).await,
        Command::History(h) => match h.action {
            Some(HistoryAction::Show(s)) => history_cmd::show(&layout, s).await,
            None => history_cmd::history(&layout, h.filters).await,
        },
        Command::Log(a) => log_cmd::log(&layout, a).await,
        Command::Stats(a) => read_cmd::stats(&layout, a).await,
        Command::Runners(a) => read_cmd::runners(&layout, a).await,
        Command::Up { names } => up_cmd::up(&layout, names).await,
        Command::Brokers { json } => view::brokers(&layout, json).await,
        Command::Dashboard {
            foreground,
            background,
        } => crate::dashboard::run(&layout, foreground, background).await,
        Command::Reload { role, name } => reload_cmd::reload(&layout, role, name).await,
        Command::Down {
            names,
            cancel,
            yes,
            wait,
            wait_timeout,
            ..
        } => {
            let wait = wait.then(|| wait_timeout.unwrap_or(down_cmd::DEFAULT_EXIT_WAIT));
            down_cmd::down_cli(&layout, names, cancel, yes, wait).await
        }
        Command::Policy {
            action: PolicyAction::Check { file },
        } => {
            let result = admin_cmd::policy_check(&layout, file.clone()).await;
            let mut tail: Vec<String> = vec!["policy".into(), "check".into()];
            tail.extend(file.map(|f| f.display().to_string()));
            broker_admin_retry(result, "policy check", &layout, &tail, no_elevate, elevated_child)
        }
        Command::Policy {
            action: PolicyAction::Reload,
        } => {
            let result = admin_cmd::policy_reload(&layout).await;
            let tail: Vec<String> = vec!["policy".into(), "reload".into()];
            broker_admin_retry(result, "policy reload", &layout, &tail, no_elevate, elevated_child)
        }
        Command::Policy { action } => policy(&layout, action, elevated_child, expect_before).await,
        Command::Config { action } => config(&layout, action, elevated_child, expect_before).await,
        Command::Secret { action } => secret(&layout, action, elevated_child).await,
        Command::Logging {
            action: LoggingAction::Reload,
        } => logging_cmd::logging_reload(&layout).await,
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
                let need = rights::Need {
                    what: "broker install-service",
                    why: "install the broker as an OS service under its own account",
                };
                let mut args = rights::layout_args(&layout);
                args.extend(["broker".into(), "install-service".into()]);
                let session = rights::session(dry_run, no_elevate, elevated_child);
                rights::run_with_rights(&need, &session, &args, || {
                    service_cmd::install_service(&layout, dry_run)
                })
            }
            BrokerAction::UpdateService { dry_run } => {
                let need = rights::Need {
                    what: "broker update-service",
                    why: "replace the broker service's copy of the binary and restart the service",
                };
                let mut args = rights::layout_args(&layout);
                args.extend(["broker".into(), "update-service".into()]);
                let session = rights::session(dry_run, no_elevate, elevated_child);
                rights::run_with_rights(&need, &session, &args, || {
                    service_cmd::update_service(dry_run)
                })
            }
            BrokerAction::Add {
                name,
                label,
                runners,
                address,
                code_file,
            } => {
                let req = lr_proto::AdminRequest::BrokerAdd {
                    name,
                    label,
                    runners,
                    address,
                };
                let mut tail: Vec<String> = vec!["broker".into(), "add".into()];
                tail.extend(broker_add_flags(&req, code_file.as_deref()));
                let result = admin_cmd::broker_add(&layout, req, code_file).await;
                broker_admin_retry(result, "broker add", &layout, &tail, no_elevate, elevated_child)
            }
        },
        Command::Runner { action } => match action {
            RunnerAction::Run {
                name,
                count,
                config,
                background,
                service,
                owner,
                job_root,
            } => {
                let how = roles_cmd::RunnerStart {
                    background,
                    service,
                    owner,
                };
                match crate::os_service::runner_service::job_root_refusal(
                    job_root.is_some(),
                    service,
                    cfg!(windows),
                ) {
                    Some(why) => Err(why),
                    None => {
                        let mut layout = layout;
                        layout.job_root = job_root;
                        roles_cmd::runner_run(&layout, name, count, config, how).await
                    }
                }
            }
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
            RunnerAction::Config { name, diff } => {
                let text = if diff {
                    runner_config_cmd::diff(&layout, &name)
                } else {
                    runner_config_cmd::show(&layout, &name)
                };
                text.map(|t| {
                    println!("{t}");
                    0
                })
            }
            RunnerAction::Outbox {
                action: OutboxAction::Drop { runner, job },
            } => runner_outbox_cmd::drop_outbox(&layout, runner, job).await,
            RunnerAction::List => {
                let result = admin_cmd::simple(&layout, lr_proto::AdminRequest::RunnerList).await;
                let tail: Vec<String> = vec!["runner".into(), "list".into()];
                broker_admin_retry(result, "runner list", &layout, &tail, no_elevate, elevated_child)
            }
            RunnerAction::Remove { name, yes } => {
                let tail: Vec<String> = vec![
                    "runner".into(),
                    "remove".into(),
                    name.clone(),
                    "--yes".into(),
                ];
                let req = lr_proto::AdminRequest::RunnerRemove { name };
                let result = admin_cmd::remove_cli(&layout, req, yes).await;
                broker_admin_retry(result, "runner remove", &layout, &tail, no_elevate, elevated_child)
            }
        },
        Command::Client { action } => match action {
            ClientAction::List => {
                let result = admin_cmd::simple(&layout, lr_proto::AdminRequest::ClientList).await;
                let tail: Vec<String> = vec!["client".into(), "list".into()];
                broker_admin_retry(result, "client list", &layout, &tail, no_elevate, elevated_child)
            }
            ClientAction::Remove { name, yes } => {
                let tail: Vec<String> = vec![
                    "client".into(),
                    "remove".into(),
                    name.clone(),
                    "--yes".into(),
                ];
                let req = lr_proto::AdminRequest::ClientRemove { name };
                let result = admin_cmd::remove_cli(&layout, req, yes).await;
                broker_admin_retry(result, "client remove", &layout, &tail, no_elevate, elevated_child)
            }
        },
        Command::Init { role } => setup::init(&layout, role),
        Command::Install {
            dir,
            role,
            allow_elevated,
            dry_run,
        } => setup::install(&layout, dir, role, allow_elevated, dry_run),
        Command::Upgrade(a) => {
            let (dry_run, relay) = (a.dry_run, upgrade_cmd::is_relay(&a));
            let code = upgrade_cmd::upgrade(&layout, a).await;
            match code {
                Ok(0)
                    if upgrade_cmd::offers_service_update(
                        relay,
                        dry_run,
                        layout.broker_is_service(),
                    ) =>
                {
                    let need = rights::Need {
                        what: "broker update-service",
                        why: "replace the broker service's copy of the binary and restart the service",
                    };
                    let mut args = rights::layout_args(&layout);
                    args.extend(["broker".into(), "update-service".into()]);
                    let session = rights::session(false, no_elevate, elevated_child);
                    Ok(rights::offer(&need, &session, &args).unwrap_or(0))
                }
                other => other,
            }
        }
        Command::Uninstall {
            purge,
            yes,
            dir,
            dry_run,
        } => setup::uninstall(&layout, purge, yes, dir, dry_run).await,
        Command::Autostart {
            switch: Switch::Status,
            role,
            json,
            ..
        } => service_cmd::autostart_status(&layout, role, json),
        Command::Autostart {
            switch,
            role,
            runner,
            service,
            dry_run,
            ..
        } => {
            let word = if switch == Switch::On { "on" } else { "off" };
            let session = rights::session(dry_run, no_elevate, elevated_child);
            let mut args = rights::layout_args(&layout);
            args.extend(["autostart".into(), word.into()]);
            match (service, role, runner.clone()) {
                (true, Some(RoleArg::Runner), Some(name)) => {
                    let need = rights::Need {
                        what: "autostart --service",
                        why: "install or remove the runner as an OS service under its own account",
                    };
                    args.extend(["runner".into(), name, "--service".into()]);
                    rights::run_with_rights(&need, &session, &args, || {
                        service_cmd::autostart(&layout, switch, role, runner, service, dry_run)
                    })
                }
                _ => {
                    let pending = !service
                        && runner.is_none()
                        && service_cmd::broker_step_pending(&layout, role, switch == Switch::On, dry_run);
                    let code =
                        service_cmd::autostart(&layout, switch, role, runner, service, dry_run);
                    match code {
                        Ok(code) if pending => {
                            let need = rights::Need {
                                what: "autostart broker",
                                why: "install or remove the broker as an OS service under its own account",
                            };
                            args.extend(["broker".into()]);
                            Ok(code.max(rights::offer(&need, &session, &args).unwrap_or(0)))
                        }
                        other => other,
                    }
                }
            }
        }
        Command::Profile { action } => profile_cmd::run(&layout, action),
        Command::Doctor { json, elevate } => {
            if elevate {
                let need = rights::Need {
                    what: "doctor --elevate",
                    why: "read the policy.toml of the broker service",
                };
                let mut args = rights::layout_args(&layout);
                args.push("doctor".into());
                let session = rights::session(false, no_elevate, elevated_child);
                match rights::run_with_rights(&need, &session, &args, || Ok(-1)) {
                    Ok(-1) => crate::doctor::run(&layout, json).await,
                    other => other,
                }
            } else {
                crate::doctor::run(&layout, json).await
            }
        }
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}
