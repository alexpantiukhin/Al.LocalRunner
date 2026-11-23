use super::change::{needs_rights, write, Change};
use super::elevate::{rerun_elevated, Elevator};
use super::target::Target;
use crate::commands::down_cmd::{self, Target as Role};
use crate::commands::logging_cmd::{self, Reload};
use crate::commands::reload_cmd::{self, Outcome};
use crate::commands::{up_cmd, RoleArg};
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
}

/// The roles a change reaches and the file system it is written to.
#[async_trait(?Send)]
pub trait RoleControl {
    /// True when writing `path` needs administrator or root rights.
    fn needs_rights(&self, path: &Path) -> bool;
    /// The clock that names the backup.
    fn clock(&self) -> &dyn Clock;
    /// Makes the roles that read the file of `target` reread it; one answer per role.
    async fn reload(
        &self,
        layout: &Layout,
        target: &Target,
    ) -> Result<Vec<(String, Outcome)>, String>;
    /// The command a user runs to restart `role`.
    fn restart_command(&self, layout: &Layout, role: &Role) -> String;
    /// Restarts `role`; the exit code of the restart.
    async fn restart(&self, layout: &Layout, role: &Role) -> Result<i32, String>;
}

/// The roles of this machine: the reload channels of the roles, the OS service manager for a
/// role that runs as a service, `down` and `up` for a role that runs as a user process.
pub struct LiveRoles;

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

fn process_restart(role: &Role) -> (Vec<String>, RoleArg) {
    match role {
        Role::Broker => (vec!["broker".into()], RoleArg::Broker),
        Role::Hub => (vec!["hub".into()], RoleArg::Hub),
        Role::Runner(name) => (vec![name.clone()], RoleArg::Runner),
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
    ) -> Result<Vec<(String, Outcome)>, String> {
        if let Target::Logging = target {
            return Ok(logging_cmd::reload_all(layout)
                .await?
                .into_iter()
                .map(|(who, answer)| (who, from_logging(answer)))
                .collect());
        }
        let Some(role) = role_of(target) else {
            return Ok(Vec::new());
        };
        let answer = reload_cmd::reload_one(layout, &role).await?;
        Ok(vec![(role_name(&role), answer)])
    }

    fn restart_command(&self, layout: &Layout, role: &Role) -> String {
        service_of(layout, role).unwrap_or_else(|| {
            let (names, up) = process_restart(role);
            let up = match up {
                RoleArg::Broker => "broker",
                RoleArg::Hub => "hub",
                _ => "runner",
            };
            format!(
                "localrunner down {} --wait; localrunner up {up}",
                names.join(" ")
            )
        })
    }

    async fn restart(&self, layout: &Layout, role: &Role) -> Result<i32, String> {
        if let Some(line) = service_of(layout, role) {
            return tokio::task::spawn_blocking(move || run_shell_line(&line))
                .await
                .map_err(|e| e.to_string())?;
        }
        let (names, up) = process_restart(role);
        let wait = Some(down_cmd::DEFAULT_EXIT_WAIT);
        let code = down_cmd::down_cli(layout, names, false, true, wait).await?;
        if code != 0 {
            return Ok(code);
        }
        up_cmd::up(layout, Some(up)).await
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

/// The restart-only keys of [`RESTART_ONLY`] whose value differs between the text before and
/// after the change, each with its reason and the value written (`None`: removed).
pub fn restart_only_changes(
    target: &Target,
    change: &Change,
) -> Vec<(&'static str, &'static str, Option<String>)> {
    let kind = TargetKind::of(target);
    let (before, after) = (table(&change.before), table(&change.after));
    RESTART_ONLY
        .iter()
        .filter(|(k, _, _)| *k == kind)
        .filter_map(|(_, key, reason)| {
            let old = before.as_ref().and_then(|t| lookup(t, key));
            let new = after.as_ref().and_then(|t| lookup(t, key));
            (before.is_none() || after.is_none() || old != new)
                .then(|| (*key, *reason, new.map(ToString::to_string)))
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

fn show(out: &mut dyn Write, change: &Change) -> std::io::Result<()> {
    write!(out, "{}", change.diff())?;
    for w in &change.warnings {
        writeln!(out, "warning: {w}")?;
    }
    for key in &change.widens {
        writeln!(
            out,
            "widens privileges: {key} = true; the role allows more than before"
        )?;
    }
    Ok(())
}

/// Applies a planned change: prints the file and, outside the elevated child, the diff, the
/// warnings and a `widens privileges` line per widening key; `dry_run` stops there. A file
/// that needs rights is left to the elevated re-run of the command, whose exit code is
/// returned. Otherwise the file is written, the roles that read it reload it and print their
/// answer, and a key that applies only at the next start names the restart command, which
/// runs with `restart`. The exit code is 1 when a role refused the file or a restart failed.
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
    let io = |e: std::io::Error| e.to_string();
    let path = change.path.display().to_string();
    if !opts.elevated_child {
        writeln!(out, "file: {path}").map_err(io)?;
    }
    if change.is_noop() {
        writeln!(out, "no change: {path} already has this value").map_err(io)?;
        return Ok(0);
    }
    if !opts.elevated_child {
        show(out, &change).map_err(io)?;
    }
    if opts.dry_run {
        writeln!(out, "dry run: nothing written").map_err(io)?;
        return Ok(0);
    }
    if !opts.elevated_child && roles.needs_rights(&change.path) {
        writeln!(
            out,
            "writing {path} needs administrator rights: running the command elevated"
        )
        .map_err(io)?;
        let exe = std::env::current_exe().map_err(|e| format!("cannot find this binary: {e}"))?;
        return Ok(rerun_elevated(
            elevator,
            Os::current(),
            &exe,
            &opts.args,
            err,
        ));
    }
    let backup = write(&change, roles.clock())?;
    if backup == change.path {
        writeln!(out, "written: {path}").map_err(io)?;
    } else {
        writeln!(out, "written: {path} (backup: {})", backup.display()).map_err(io)?;
    }
    let mut code = 0;
    let mut to_restart: Vec<Role> = Vec::new();
    for (who, answer) in roles.reload(layout, target).await? {
        match answer {
            Outcome::NotRunning => writeln!(out, "{who}: not running").map_err(io)?,
            Outcome::Reloaded { restart_required } => {
                writeln!(out, "{who}: reloaded").map_err(io)?;
                for setting in &restart_required {
                    writeln!(out, "{who}: needs a restart: {setting}").map_err(io)?;
                }
                if !restart_required.is_empty() {
                    if let Target::Runner(name) = target {
                        to_restart.push(Role::Runner(name.clone()));
                    }
                }
            }
            Outcome::Refused(why) => {
                code = 1;
                writeln!(err, "{who}: {why}").map_err(io)?;
            }
        }
    }
    let pending = restart_only_changes(target, &change);
    if let Some(role) = role_of(target) {
        let name = role_name(&role);
        for (key, reason, value) in &pending {
            let value = match value {
                Some(v) => format!("{key} = {v} written"),
                None => format!("{key} removed"),
            };
            writeln!(
                out,
                "{value}; it applies at the next start of {name}: {reason}"
            )
            .map_err(io)?;
        }
        if !pending.is_empty() && !to_restart.contains(&role) {
            to_restart.push(role);
        }
    }
    for role in to_restart {
        let name = role_name(&role);
        let command = roles.restart_command(layout, &role);
        if !opts.restart {
            writeln!(
                out,
                "restart {name} to apply it (its connections drop): {command}"
            )
            .map_err(io)?;
            continue;
        }
        writeln!(
            out,
            "restarting {name}: its connections drop and reconnect ({command})"
        )
        .map_err(io)?;
        match roles.restart(layout, &role).await {
            Ok(0) => writeln!(out, "{name}: restarted").map_err(io)?,
            Ok(c) => {
                code = 1;
                writeln!(
                    err,
                    "{name}: the restart ended with exit code {c}; run it by hand: {command}"
                )
                .map_err(io)?;
            }
            Err(e) => {
                code = 1;
                writeln!(
                    err,
                    "{name}: cannot restart ({e}); run it by hand: {command}"
                )
                .map_err(io)?;
            }
        }
    }
    Ok(code)
}
