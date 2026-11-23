use super::change::{needs_rights, write, Change};
use super::elevate::{command_words, rerun_elevated, Elevator};
use super::target::Target;
use crate::commands::down_cmd::{self, Target as Role};
use crate::commands::logging_cmd::{self, Reload};
use crate::commands::reload_cmd::{self, Outcome};
use crate::commands::up_cmd;
use crate::os_service::{runner_service, state, Os};
use crate::paths::Layout;
use async_trait::async_trait;
use lr_abstractions::Clock;
use std::io::Write;
use std::path::Path;

/// The kind of a [`Target`], without the runner name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    /// `policy.toml`.
    Broker,
    /// `hub-policy.toml`.
    Hub,
    /// A `runner.toml`.
    Runner,
    /// `logging.toml`.
    Logging,
}

impl TargetKind {
    /// The kind of `target`.
    pub fn of(target: &Target) -> TargetKind {
        match target {
            Target::Broker => TargetKind::Broker,
            Target::Hub => TargetKind::Hub,
            Target::Runner(_) => TargetKind::Runner,
            Target::Logging => TargetKind::Logging,
        }
    }
}

/// Keys a running role does not take on reload: the kind of file, the dotted key (a table
/// covers every key under it) and why the change waits for the next start.
pub const RESTART_ONLY: &[(TargetKind, &str, &str)] = &[
    (
        TargetKind::Runner,
        "allow_elevated",
        "a runner checks it only when it starts",
    ),
    (
        TargetKind::Broker,
        "listen",
        "the broker binds its addresses when it starts",
    ),
    (
        TargetKind::Broker,
        "networks",
        "the broker reads its networks when it starts",
    ),
    (
        TargetKind::Broker,
        "shared",
        "the broker reads its shared sets when it starts",
    ),
];

/// How [`run_change`] runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyOpts {
    /// Print the change and write nothing.
    pub dry_run: bool,
    /// Restart the role whose changed key applies only at its next start.
    pub restart: bool,
    /// This process is the elevated re-run: the diff is not shown again and it never elevates.
    pub elevated_child: bool,
    /// The arguments of the elevated re-run after the program, without the child marker.
    pub args: Vec<String>,
    /// The dotted keys the command edits; a restart-only key among them counts even when the
    /// file already had the value.
    pub keys: Vec<String>,
}

/// The roles a change reaches and the file system it is written to.
#[async_trait(?Send)]
pub trait RoleControl {
    /// True when writing `path` needs administrator or root rights.
    fn needs_rights(&self, path: &Path) -> bool;
    /// The clock that names the backup.
    fn clock(&self) -> &dyn Clock;
    /// Makes `role` reread the file of `target`.
    async fn reload(
        &self,
        layout: &Layout,
        target: &Target,
        role: &Role,
    ) -> Result<Outcome, String>;
    /// The command a user runs to restart `role`.
    fn restart_command(&self, layout: &Layout, role: &Role) -> String;
    /// Restarts `role`; a warning goes to `err`.
    async fn restart(&self, layout: &Layout, role: &Role, err: &mut dyn Write) -> RestartOutcome;
}

/// The roles of this machine: the reload channels of the roles, the OS service manager for a
/// role that runs as a service, `down` and a start of that one role for a user process.
pub struct LiveRoles;

/// How a role is restarted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartPlan {
    /// The role is an OS service: this command of the service manager restarts it.
    Service(String),
    /// The role is a user process: `down` of these names, then a start with these arguments
    /// (without the directories).
    Process {
        /// The role stopped, typed: a runner named `runner`, `hub` or `broker` is that runner.
        stop: Role,
        /// The arguments that start this one role again.
        start: Vec<String>,
    },
}

/// The restart of `role`: `service` is the service manager's restart command when the role
/// runs as an OS service; otherwise the role is stopped and started by its own name.
pub fn restart_plan(role: &Role, service: Option<String>) -> RestartPlan {
    if let Some(line) = service {
        return RestartPlan::Service(line);
    }
    let start: Vec<String> = match role {
        Role::Broker => vec!["broker".into(), "run".into()],
        Role::Hub => vec!["hub".into(), "run".into()],
        Role::Runner(name) => vec!["runner".into(), "run".into(), "--name".into(), name.clone()],
    };
    RestartPlan::Process {
        stop: role.clone(),
        start: start
            .into_iter()
            .chain(std::iter::once("--background".to_string()))
            .collect(),
    }
}

/// `--state-dir` and `--config-dir` of `layout` when they differ from the defaults.
pub fn explicit_dirs(layout: &Layout) -> Vec<String> {
    let default = crate::paths::default_root();
    let mut out = Vec::new();
    if default.as_ref() != Some(&layout.root) || layout.config != layout.root {
        out.extend(["--state-dir".to_string(), layout.root.display().to_string()]);
    }
    if layout.config != layout.root {
        out.extend([
            "--config-dir".to_string(),
            layout.config.display().to_string(),
        ]);
    }
    out
}

/// The command a user types for `plan`, every word quoted for the shell of `os` when needed.
pub fn restart_line(os: Os, layout: &Layout, plan: &RestartPlan) -> String {
    match plan {
        RestartPlan::Service(line) => line.clone(),
        RestartPlan::Process { stop, start } => {
            let dirs = explicit_dirs(layout);
            let program = std::iter::once("localrunner".to_string());
            let down: Vec<String> = program
                .clone()
                .chain(dirs.iter().cloned())
                .chain(std::iter::once("down".to_string()))
                .chain(std::iter::once(role_name(stop)))
                .chain(std::iter::once("--wait".to_string()))
                .collect();
            let up: Vec<String> = program
                .chain(dirs.iter().cloned())
                .chain(start.iter().cloned())
                .collect();
            format!("{}; {}", command_words(os, &down), command_words(os, &up))
        }
    }
}

fn service_of(layout: &Layout, role: &Role) -> Option<String> {
    let os = Os::current();
    match role {
        Role::Broker if layout.broker_is_service() => Some(state::restart_command(os)),
        Role::Runner(name) if runner_service::installed(os, name) => Some(
            state::named_restart_command(os, &runner_service::service_name(os, name)),
        ),
        _ => None,
    }
}

fn run_shell_line(line: &str) -> Result<i32, String> {
    let (program, args): (&str, Vec<&str>) = if cfg!(windows) {
        (
            "powershell.exe",
            vec!["-NoProfile", "-NonInteractive", "-Command", line],
        )
    } else {
        ("sh", vec!["-c", line])
    };
    let path = crate::os_service::programs::program_path(program)?;
    let status = std::process::Command::new(path)
        .args(args)
        .status()
        .map_err(|e| format!("cannot run {line}: {e}"))?;
    status
        .code()
        .ok_or_else(|| format!("{line} was stopped by a signal"))
}

fn from_logging(answer: Reload) -> Outcome {
    match answer {
        Reload::NotRunning => Outcome::NotRunning,
        Reload::Reloaded => Outcome::Reloaded {
            restart_required: Vec::new(),
        },
        Reload::Refused(why) => Outcome::Refused(why),
    }
}

#[async_trait(?Send)]
impl RoleControl for LiveRoles {
    fn needs_rights(&self, path: &Path) -> bool {
        needs_rights(path)
    }

    fn clock(&self) -> &dyn Clock {
        &lr_exec::SystemClock
    }

    async fn reload(
        &self,
        layout: &Layout,
        target: &Target,
        role: &Role,
    ) -> Result<Outcome, String> {
        match target {
            Target::Logging => logging_cmd::reload_one(layout, role)
                .await
                .map(from_logging),
            _ => reload_cmd::reload_one(layout, role).await,
        }
    }

    fn restart_command(&self, layout: &Layout, role: &Role) -> String {
        restart_line(
            Os::current(),
            layout,
            &restart_plan(role, service_of(layout, role)),
        )
    }

    async fn restart(&self, layout: &Layout, role: &Role, err: &mut dyn Write) -> RestartOutcome {
        match restart_plan(role, service_of(layout, role)) {
            RestartPlan::Service(line) => {
                restart_service(&LiveProcesses, layout, role, &line, up_cmd::STARTED_WITHIN).await
            }
            RestartPlan::Process { stop, start } => {
                restart_process(
                    &LiveProcesses,
                    layout,
                    &stop,
                    &start,
                    up_cmd::STARTED_WITHIN,
                    err,
                )
                .await
            }
        }
    }
}

/// What a restart came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartOutcome {
    /// The role was started again and answers on its channel.
    Restarted,
    /// The role was started again but did not answer within the wait.
    NotAnswering,
    /// The service manager restarted the service but the role did not answer within the wait.
    ServiceNotAnswering,
    /// The start was refused before anything was stopped, with `up`'s reason.
    Refused(String),
    /// Stopping or starting failed.
    Failed(String),
}

/// Stopping, starting and probing one role that runs as a user process.
#[async_trait(?Send)]
pub trait RoleProcesses {
    /// The check `up` makes before it starts `role`: `Ok(Some(risk))` warns, `Err` refuses.
    fn start_verdict(&self, layout: &Layout, role: &Role) -> Result<Option<String>, String>;
    /// Stops exactly `role` and waits until it has exited; the exit code of that stop.
    async fn stop(&self, layout: &Layout, role: &Role) -> Result<i32, String>;
    /// Starts the role with `argv` (without the directories) in the background.
    fn start(&self, layout: &Layout, argv: &[String]) -> Result<(), String>;
    /// Whether `role` answers on its channel.
    async fn answers(&self, layout: &Layout, role: &Role) -> bool;
    /// Runs the service manager's restart command `line`; its exit code.
    async fn run_service_restart(&self, line: &str) -> Result<i32, String>;
}

/// The processes of this machine: `up`'s same-user check and readiness probe, `down` of one
/// typed role, and a background start of this binary.
pub struct LiveProcesses;

#[async_trait(?Send)]
impl RoleProcesses for LiveProcesses {
    fn start_verdict(&self, layout: &Layout, role: &Role) -> Result<Option<String>, String> {
        match role {
            Role::Runner(name) => up_cmd::runner_start_verdict(layout, name),
            _ => Ok(None),
        }
    }

    async fn stop(&self, layout: &Layout, role: &Role) -> Result<i32, String> {
        down_cmd::stop_one(layout, role.clone(), down_cmd::DEFAULT_EXIT_WAIT).await
    }

    fn start(&self, layout: &Layout, argv: &[String]) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| format!("cannot find this binary: {e}"))?;
        let argv: Vec<String> = argv.iter().cloned().chain(up_cmd::dirs(layout)).collect();
        up_cmd::starter(exe)(&argv)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn answers(&self, layout: &Layout, role: &Role) -> bool {
        up_cmd::role_answers(layout, role).await
    }

    async fn run_service_restart(&self, line: &str) -> Result<i32, String> {
        let line = line.to_string();
        tokio::task::spawn_blocking(move || run_shell_line(&line))
            .await
            .map_err(|e| e.to_string())?
    }
}

/// Restarts a role that runs as an OS service with the service manager's command `line`, then
/// waits as `up` does, at most `within`, until the role answers on its channel.
pub async fn restart_service(
    procs: &dyn RoleProcesses,
    layout: &Layout,
    role: &Role,
    line: &str,
    within: std::time::Duration,
) -> RestartOutcome {
    match procs.run_service_restart(line).await {
        Ok(0) => {}
        Ok(c) => return RestartOutcome::Failed(format!("exit code {c}")),
        Err(e) => return RestartOutcome::Failed(e),
    }
    let answered = up_cmd::poll_until(|| procs.answers(layout, role), |a: &bool| *a, within).await;
    match answered {
        true => RestartOutcome::Restarted,
        false => RestartOutcome::ServiceNotAnswering,
    }
}

/// Restarts one user-process role: `up`'s start check first (a refusal stops nothing), then a
/// stop of exactly that role, a start with `start`, and `up`'s wait of at most `within` for
/// the role to answer. A warning of the check goes to `err`.
pub async fn restart_process(
    procs: &dyn RoleProcesses,
    layout: &Layout,
    role: &Role,
    start: &[String],
    within: std::time::Duration,
    err: &mut dyn Write,
) -> RestartOutcome {
    match procs.start_verdict(layout, role) {
        Ok(None) => {}
        Ok(Some(risk)) => {
            let _ = writeln!(err, "warning: {risk}");
        }
        Err(refusal) => return RestartOutcome::Refused(refusal),
    }
    match procs.stop(layout, role).await {
        Ok(0) => {}
        Ok(c) => return RestartOutcome::Failed(format!("the stop ended with exit code {c}")),
        Err(e) => return RestartOutcome::Failed(e),
    }
    if let Err(e) = procs.start(layout, start) {
        return RestartOutcome::Failed(format!("cannot start it: {e}"));
    }
    let answered = up_cmd::poll_until(|| procs.answers(layout, role), |a: &bool| *a, within).await;
    match answered {
        true => RestartOutcome::Restarted,
        false => RestartOutcome::NotAnswering,
    }
}

fn role_of(target: &Target) -> Option<Role> {
    match target {
        Target::Broker => Some(Role::Broker),
        Target::Hub => Some(Role::Hub),
        Target::Runner(name) => Some(Role::Runner(name.clone())),
        Target::Logging => None,
    }
}

/// The roles that read the file of `target`: its own role, or for `logging` every role of the
/// installation (runners, the hub, the broker).
pub fn roles_of(layout: &Layout, target: &Target) -> Vec<Role> {
    match role_of(target) {
        Some(role) => vec![role],
        None => down_cmd::down_targets(layout, &[]),
    }
}

fn role_name(role: &Role) -> String {
    match role {
        Role::Broker => "broker".into(),
        Role::Hub => "hub".into(),
        Role::Runner(name) => name.clone(),
    }
}

fn table(text: &str) -> Option<toml::Table> {
    text.parse::<toml::Table>().ok()
}

fn lookup<'a>(table: &'a toml::Table, key: &str) -> Option<&'a toml::Value> {
    let mut parts = key.split('.');
    let mut value = table.get(parts.next()?)?;
    for part in parts {
        value = value.as_table()?.get(part)?;
    }
    Some(value)
}

fn touches(edited: &str, prefix: &str) -> bool {
    edited == prefix
        || edited.starts_with(&format!("{prefix}."))
        || prefix.starts_with(&format!("{edited}."))
}

/// The keys of [`RESTART_ONLY`] that the change touches: their value differs between the
/// text before and after, or one of `keys` lies in them. Each comes with its reason and the
/// value in the file after the change (`None`: absent).
pub fn restart_only_changes(
    target: &Target,
    change: &Change,
    keys: &[String],
) -> Vec<(&'static str, &'static str, Option<String>)> {
    let kind = TargetKind::of(target);
    let (before, after) = (table(&change.before), table(&change.after));
    RESTART_ONLY
        .iter()
        .filter(|(k, _, _)| *k == kind)
        .filter_map(|(_, key, reason)| {
            let old = before.as_ref().and_then(|t| lookup(t, key));
            let new = after.as_ref().and_then(|t| lookup(t, key));
            let differs = before.is_none() || after.is_none() || old != new;
            let edited = keys.iter().any(|k| touches(k, key));
            (differs || edited).then(|| (*key, *reason, new.map(ToString::to_string)))
        })
        .collect()
}

/// [`run_change_with`] on the roles of this machine, printing to stdout and stderr.
pub async fn run_change(
    layout: &Layout,
    target: &Target,
    change: Change,
    opts: ApplyOpts,
    elevator: &dyn Elevator,
) -> Result<i32, String> {
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    run_change_with(
        layout, target, change, opts, elevator, &LiveRoles, &mut out, &mut err,
    )
    .await
}

fn show(out: &mut dyn Write, change: &Change) {
    let _ = write!(out, "{}", change.diff());
    for w in &change.warnings {
        let _ = writeln!(out, "warning: {w}");
    }
    for key in &change.widens {
        let _ = writeln!(
            out,
            "widens privileges: {key} = true; the role allows more than before"
        );
    }
}

/// The exit code of an elevated child that wrote the file but whose reload or restart of the
/// broker failed; any other non-zero code of the child means nothing was written.
pub const EXIT_WRITTEN_STEP_FAILED: i32 = 75;

/// Which roles a process reloads and restarts after the write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Every role: the process wrote the file with its own rights.
    All,
    /// Only the broker, whose admin channel has fixed names: the elevated child.
    Elevated,
    /// Every role but the broker, with the user's own layout: the parent after its elevated
    /// child succeeded.
    User,
}

impl Phase {
    /// Whether this phase reloads and restarts `role`.
    pub fn covers(self, role: &Role) -> bool {
        match self {
            Phase::All => true,
            Phase::Elevated => *role == Role::Broker,
            Phase::User => *role != Role::Broker,
        }
    }
}

/// Applies a planned change. Outside the elevated child it prints the file and the diff, the
/// warnings and a `widens privileges` line per widening key, or `no change in <path>`;
/// `dry_run` stops there. A file that needs rights is written by the elevated re-run of the
/// command, which also reloads and restarts the broker; when it exits 0 this process reloads
/// and restarts the other roles ([`Phase::User`]), otherwise its exit code is returned and
/// nothing else happens. Without rights the file is written here and every role is handled.
/// After the write nothing aborts: every role's answer, the restart-only notes and the restart
/// commands are printed, and the exit code is 1 when a reload or a restart failed.
#[allow(clippy::too_many_arguments)]
pub async fn run_change_with(
    layout: &Layout,
    target: &Target,
    change: Change,
    opts: ApplyOpts,
    elevator: &dyn Elevator,
    roles: &dyn RoleControl,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, String> {
    let path = change.path.display().to_string();
    let noop = change.is_noop();
    if !opts.elevated_child {
        let _ = writeln!(out, "file: {path}");
        if let Target::Runner(name) = target {
            let own = layout.runner_dir(name).join("runner.toml");
            let kind = if change.path == own {
                "its own copy; edits of the template do not reach it"
            } else {
                "edits the template: every runner without an own copy reads it"
            };
            let _ = writeln!(out, "runner {name}: {kind}");
        }
        match noop {
            true => {
                let _ = writeln!(out, "no change in {path}");
            }
            false => show(out, &change),
        }
    }
    if opts.dry_run {
        let _ = writeln!(out, "dry run: nothing written");
        return Ok(0);
    }
    if !opts.elevated_child && roles.needs_rights(&change.path) {
        let _ = writeln!(
            out,
            "writing {path} needs administrator rights: running the command elevated"
        );
        let exe = std::env::current_exe().map_err(|e| format!("cannot find this binary: {e}"))?;
        let code = rerun_elevated(elevator, Os::current(), &exe, &opts.args, err);
        if code != 0 && code != EXIT_WRITTEN_STEP_FAILED {
            return Ok(code);
        }
        let user = after_write(Phase::User, layout, target, &change, &opts, roles, out, err).await;
        return Ok(if code == 0 { user } else { 1 });
    }
    if !noop {
        let backup = write(&change, roles.clock())?;
        let _ = match backup == change.path {
            true => writeln!(out, "written: {path}"),
            false => writeln!(out, "written: {path} (backup: {})", backup.display()),
        };
    }
    if opts.elevated_child {
        let code = after_write(
            Phase::Elevated,
            layout,
            target,
            &change,
            &opts,
            roles,
            out,
            err,
        )
        .await;
        return Ok(if code == 0 {
            0
        } else {
            EXIT_WRITTEN_STEP_FAILED
        });
    }
    Ok(after_write(Phase::All, layout, target, &change, &opts, roles, out, err).await)
}

/// The reload and restart phase after the file is written: reloads the roles of `target` that
/// `phase` covers, prints each answer, the restart-only notes and the restart commands, and
/// restarts with `opts.restart`. Nothing aborts it; the result is 1 when anything failed.
#[allow(clippy::too_many_arguments)]
pub async fn after_write(
    phase: Phase,
    layout: &Layout,
    target: &Target,
    change: &Change,
    opts: &ApplyOpts,
    roles: &dyn RoleControl,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let mut code = 0;
    let mut to_restart: Vec<Role> = Vec::new();
    for role in roles_of(layout, target)
        .into_iter()
        .filter(|r| phase.covers(r))
    {
        let who = role_name(&role);
        match roles.reload(layout, target, &role).await {
            Ok(Outcome::NotRunning) => {
                let _ = writeln!(out, "{who}: not running");
            }
            Ok(Outcome::Reloaded { restart_required }) => {
                let _ = writeln!(out, "{who}: reloaded");
                for setting in &restart_required {
                    let _ = writeln!(out, "{who}: needs a restart: {setting}");
                }
                if !restart_required.is_empty() && role_of(target).as_ref() == Some(&role) {
                    to_restart.push(role);
                }
            }
            Ok(Outcome::Refused(why)) => {
                code = 1;
                let _ = writeln!(err, "{who}: {why}");
            }
            Err(e) => {
                code = 1;
                let _ = writeln!(err, "{who}: cannot reload: {e}");
            }
        }
    }
    if let Some(role) = role_of(target).filter(|r| phase.covers(r)) {
        let name = role_name(&role);
        let pending = restart_only_changes(target, change, &opts.keys);
        for (key, reason, value) in &pending {
            let value = match value {
                Some(v) => format!("{key} = {v}"),
                None => format!("{key} unset"),
            };
            let _ = writeln!(
                out,
                "{value}: it applies at the next start of {name}: {reason}"
            );
        }
        if !pending.is_empty() && !to_restart.contains(&role) {
            to_restart.push(role);
        }
    }
    for role in to_restart {
        let name = role_name(&role);
        let command = roles.restart_command(layout, &role);
        if !opts.restart {
            let _ = writeln!(
                out,
                "restart {name} to apply it (its connections drop): {command}"
            );
            continue;
        }
        let _ = writeln!(
            out,
            "restarting {name}: its connections drop and reconnect ({command})"
        );
        let outcome = roles.restart(layout, &role, err).await;
        let line = match outcome {
            RestartOutcome::Restarted => {
                let _ = writeln!(out, "{name}: restarted");
                continue;
            }
            RestartOutcome::NotAnswering => format!(
                "{name}: started but not answering within {} s; see logs/ in {}",
                up_cmd::STARTED_WITHIN.as_secs(),
                layout.root.display()
            ),
            RestartOutcome::ServiceNotAnswering => format!(
                "{name}: service restarted but not answering within {} s; check it: {command}",
                up_cmd::STARTED_WITHIN.as_secs()
            ),
            RestartOutcome::Refused(why) => format!("runner {name}: not started: {why}"),
            RestartOutcome::Failed(e) => {
                format!("{name}: cannot restart ({e}); run it by hand: {command}")
            }
        };
        code = 1;
        let _ = writeln!(err, "{line}");
    }
    code
}
