//! `upgrade [--dir] [--yes] [--dry-run]` (spec 10.5): run by the NEW binary, it replaces the
//! installed one. It tells the installed and the new version, stops the roles of this
//! installation that run (runners drain, then the hub and a broker started by hand), copies
//! itself over the installed binary and starts the same roles again from it. The broker OS
//! service keeps its own copy of the binary and is left alone.

use super::args::{RoleArg, UpgradeArgs};
use super::confirm::{Console, Stdio};
use super::down_cmd::{down_for, down_mode, wait_for_runners, IpcRoles, Roles, Waited};
use super::setup::{binary_name, default_bin_dir, upgrade_plan};
use super::up_cmd::{running_roles, starter, up_with, up_with_services, RunningRoles, Spawn};
use super::upgrade_jobs::{activity, choose, Activity, Mode, WaitProgress};
use super::upgrade_pool::{plan_lines, pool, reconnected, report_lines, PoolRunner, Reconnected};
use crate::os_service::{apply_all, Action, DryRun, OsSystem, System};
use crate::paths::Layout;
use crate::render::{Card, Level, Out, Report};
use crate::upgrade_lock::{write, UpgradeLock};
use lr_proto::DrainReason;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

const STARTED_WITHIN: Duration = Duration::from_secs(15);
const BUSY_FOR: Duration = Duration::from_secs(20);
const JOBS_REFRESH: Duration = Duration::from_secs(5);

/// How long `upgrade` waits for the runners it restarted to reconnect to their broker before it
/// reports; longer than a heartbeat.
pub const RECONNECT_WITHIN: Duration = Duration::from_secs(20);
const RECONNECT_POLL: Duration = Duration::from_millis(200);

/// Printed with the plan: the old file is renamed, not overwritten.
pub const KEEP_OLD_NOTE: &str =
    "clients of other sessions that hold the old file keep running on the old version";

/// `--wait-timeout`: a whole number of seconds, minutes or hours (`90s`, `15m`, `2h`), not zero.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let bad = || format!("`{text}` is not a duration: use `90s`, `15m` or `2h`");
    let (digits, unit) = [("s", 1u64), ("m", 60), ("h", 3600)]
        .into_iter()
        .find_map(|(suffix, unit)| text.strip_suffix(suffix).map(|d| (d, unit)))
        .ok_or_else(bad)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: u64 = digits.parse().map_err(|_| bad())?;
    match n.checked_mul(unit) {
        Some(0) => Err(format!("`{text}` is zero: give a duration above zero")),
        Some(secs) => Ok(Duration::from_secs(secs)),
        None => Err(bad()),
    }
}

/// What a version line says: `localrunner 0.1.1 (protocol 1.0)` or `0.1.1 (protocol 1.0)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// The numbers of `X.Y.Z`.
    pub number: (u64, u64, u64),
    /// The version text as printed, with a suffix such as `-rc1`.
    pub text: String,
    /// The major protocol version, when printed.
    pub protocol_major: Option<u32>,
}

/// Reads a version line; `None` when it holds no `X.Y.Z`.
pub fn parse_version(line: &str) -> Option<Version> {
    let word = line
        .split_whitespace()
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let core = word.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let number = (parts.next()??, parts.next()??, parts.next()??);
    let protocol_major = line
        .split_once("protocol ")
        .and_then(|(_, rest)| rest.split(['.', ')']).next())
        .and_then(|m| m.trim().parse().ok());
    Some(Version {
        number,
        text: word.to_string(),
        protocol_major,
    })
}

/// What the new binary means for the installed one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The same build: nothing to do.
    UpToDate,
    /// The new binary is older: database schemas may not go back.
    Downgrade,
    /// Replace; `protocol_changed` when the major protocol versions differ.
    Upgrade {
        /// Other nodes must be upgraded in the same window.
        protocol_changed: bool,
    },
}

/// Compares the installed version with the new one; an installed binary that printed no
/// version is replaced.
pub fn verdict(installed: Option<&Version>, new: &Version) -> Verdict {
    let Some(old) = installed else {
        return Verdict::Upgrade {
            protocol_changed: false,
        };
    };
    if old.text == new.text && old.protocol_major == new.protocol_major {
        return Verdict::UpToDate;
    }
    if new.number < old.number {
        return Verdict::Downgrade;
    }
    Verdict::Upgrade {
        protocol_changed: old.protocol_major != new.protocol_major,
    }
}

/// The roles of `running` that `upgrade` stops, as `down` names them; a broker OS service
/// stays.
pub fn stop_names(running: &RunningRoles, service: bool) -> Vec<String> {
    let mut out: Vec<String> = running.runners.iter().cloned().collect();
    if running.hub {
        out.push("hub".into());
    }
    if running.broker && !service {
        out.push("broker".into());
    }
    out
}

/// The roles started again after the replacement, in `up`'s order.
pub fn restart_roles(running: &RunningRoles, service: bool) -> Vec<RoleArg> {
    let mut out = Vec::new();
    if running.broker && !service {
        out.push(RoleArg::Broker);
    }
    if running.hub {
        out.push(RoleArg::Hub);
    }
    if !running.runners.is_empty() {
        out.push(RoleArg::Runner);
    }
    out
}

/// The roles started again as the plan prints them: the runners by name.
pub fn restart_summary(running: &RunningRoles, service: bool) -> String {
    restart_roles(running, service)
        .iter()
        .map(|r| match r {
            RoleArg::Runner => {
                let names: Vec<&str> = running.runners.iter().map(String::as_str).collect();
                format!("runner ({})", names.join(", "))
            }
            other => format!("{other:?}").to_lowercase(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Applies the copies one by one; a stopped role may hold its binary for a few seconds more, so
/// a failing copy is retried for [`BUSY_FOR`] before it is reported. A copy that succeeded is
/// not applied again: it would replace the saved old file with the new binary.
pub async fn replace_binary(system: &dyn System, copies: &[Action]) -> Result<(), String> {
    let start = tokio::time::Instant::now();
    for copy in copies {
        loop {
            match apply_all(system, std::slice::from_ref(copy)) {
                Err(e) if start.elapsed() >= BUSY_FOR => return Err(e),
                Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
                Ok(()) => break,
            }
        }
    }
    Ok(())
}

/// The version `path --version` prints; `None` when it does not run or prints none.
pub fn installed_version(path: &Path) -> Option<Version> {
    let out = std::process::Command::new(path)
        .arg("--version")
        .output()
        .ok()?;
    parse_version(&String::from_utf8_lossy(&out.stdout))
}

/// A starter of roles for `upgrade`: a role that has a systemd user unit (`autostart on`) is
/// started through `systemctl --user start`, and one that has a launchd agent on macOS through
/// `launchctl start`, so it gets the environment of its unit or agent and the service manager
/// keeps owning it; any other role is started as a detached process.
pub fn unit_starter(exe: PathBuf) -> impl Fn(&[String]) -> std::io::Result<u32> + Sync {
    let plain = starter(exe);
    let owner = std::process::id();
    move |argv: &[String]| {
        if cfg!(target_os = "linux") {
            let unit = crate::os_service::autostart::unit_for_argv(argv);
            let file = unit.as_ref().and_then(|u| {
                crate::os_service::home_dir()
                    .ok()
                    .map(|h| h.join(".config/systemd/user").join(u))
            });
            if let (Some(unit), Some(file)) = (unit, file) {
                if file.is_file() {
                    let status = with_owner_env("linux", owner, || {
                        std::process::Command::new("systemctl")
                            .args(["--user", "start", &unit])
                            .status()
                    })?;
                    return if status.success() {
                        Ok(0)
                    } else {
                        Err(std::io::Error::other(format!(
                            "systemctl --user start {unit} failed"
                        )))
                    };
                }
            }
        }
        if cfg!(target_os = "macos") {
            let agent = crate::os_service::home_dir()
                .ok()
                .and_then(|h| crate::os_service::autostart::agent_for_argv(argv, &h));
            if let Some((label, _)) = agent.filter(|(_, p)| p.is_file()) {
                let status = with_owner_env("macos", owner, || {
                    std::process::Command::new("launchctl")
                        .args(["start", &label])
                        .status()
                })?;
                return if status.success() {
                    Ok(0)
                } else {
                    Err(std::io::Error::other(format!(
                        "launchctl start {label} failed"
                    )))
                };
            }
        }
        plain(argv)
    }
}

/// One program and its arguments.
pub type Command = (String, Vec<String>);

/// The commands that set (and, where it is safe, clear) `LOCALRUNNER_UPGRADE_LOCK_PID`, the pid
/// of the upgrade that owns the lock, in the environment of the service manager of `os`
/// (`linux`: the systemd user manager, `macos`: launchd). A unit or an agent does not inherit
/// the environment of the process that starts it, and a role without this variable takes the
/// lock of its own upgrade for a foreign one, starts in drain and leaves. systemd reads the
/// environment when it spawns the unit, which is done when `systemctl start` returns, so the
/// variable is cleared right after; `launchctl start` returns before the spawn, so the agent
/// keeps the variable, a pid that matters only for the lock of that one upgrade. `None` for
/// another OS.
pub fn owner_env_commands(os: &str, pid: u32) -> Option<(Command, Option<Command>)> {
    let key = crate::upgrade_lock::OWNER_ENV;
    let words = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match os {
        "linux" => Some((
            (
                "systemctl".into(),
                words(&["--user", "set-environment", &format!("{key}={pid}")]),
            ),
            Some((
                "systemctl".into(),
                words(&["--user", "unset-environment", key]),
            )),
        )),
        "macos" => Some((
            ("launchctl".into(), words(&["setenv", key, &pid.to_string()])),
            None,
        )),
        _ => None,
    }
}

/// Runs `start` with the owner variable set in the service manager (see
/// [`owner_env_commands`]) and clears it afterwards, also when `start` fails.
fn with_owner_env<T>(
    os: &str,
    pid: u32,
    start: impl FnOnce() -> std::io::Result<T>,
) -> std::io::Result<T> {
    let Some((set, unset)) = owner_env_commands(os, pid) else {
        return start();
    };
    let run = |(program, args): &Command| {
        let _ = std::process::Command::new(program).args(args).status();
    };
    run(&set);
    let result = start();
    if let Some(unset) = unset {
        run(&unset);
    }
    result
}

/// The warnings about the `env:` secrets of the running runners `names` that `upgrade` would
/// start without their variable: a runner restarted by `upgrade` gets the environment of the
/// shell that runs it, so a variable unset there (`env` gives its value) is lost unless the
/// runner has a service-manager unit (`has_unit`) that sets it. Names only, never values.
pub fn restart_env_warnings(
    layout: &Layout,
    names: &std::collections::BTreeSet<String>,
    has_unit: impl Fn(&str) -> bool,
    env: impl Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for name in names.iter().filter(|n| !has_unit(n)) {
        let Some(text) = crate::build::runner_config_path(layout, name, None)
            .and_then(|p| std::fs::read_to_string(p).ok())
        else {
            continue;
        };
        let Ok(table) = text.parse::<toml::Table>() else {
            continue;
        };
        for (secret, from) in crate::runner_config::secret_sources(&table) {
            if let crate::runner_config::SecretFrom::Env(var) = from {
                if env(&var).is_none_or(|v| v.is_empty()) {
                    out.push(format!(
                        "runner {name}: secret {secret} comes from the variable {var}, which is not set here; the restarted runner will not have it (store it with `localrunner secret set {name} {secret}` or start the runner through its unit)"
                    ));
                }
            }
        }
    }
    out
}

/// Whether this process only fetches the release and hands over to the downloaded binary (it
/// is the installed one). The downloaded binary does the replacing and asks for the service
/// update itself, so the installed one must not ask again.
pub fn is_relay(args: &UpgradeArgs) -> bool {
    let Ok(dir) = args.dir.clone().map_or_else(default_bin_dir, Ok) else {
        return false;
    };
    std::env::current_exe()
        .is_ok_and(|exe| super::upgrade_fetch::is_installed(&exe, &dir.join(binary_name())))
}

/// Whether `upgrade` ends with the offer to update the broker service: only the process that
/// replaced the binaries, after a real run, on a machine whose broker is a service.
pub fn offers_service_update(relay: bool, dry_run: bool, broker_is_service: bool) -> bool {
    !relay && !dry_run && broker_is_service
}

/// Whether the offer to update the broker service follows an `upgrade` that ended as
/// `result`: after a finished run, also one that ended with a warning (a role that did not come
/// back is no reason to leave the service old), not after a refusal.
pub fn offers_service_update_after(result: &Result<i32, String>) -> bool {
    result.is_ok()
}

/// The last line of `upgrade` while the broker service (`old`, the version its own copy of
/// the binary prints) is not the version being installed (`new`); `command` updates it.
pub fn service_warning(old: Option<&str>, new: &str, command: &str) -> Option<String> {
    (old != Some(new)).then(|| {
        format!(
            "warn  broker service: {}, the binary is {new}; update it: {command}",
            old.unwrap_or("version unknown")
        )
    })
}

/// `upgrade` as the command line runs it: refused as root or administrator unless
/// `allow_elevated`.
pub async fn upgrade(layout: &Layout, args: UpgradeArgs) -> Result<i32, String> {
    let UpgradeArgs {
        dir,
        yes,
        cancel,
        allow_elevated,
        dry_run,
        wait_timeout,
        ..
    } = args.clone();
    if crate::host::is_elevated() && !allow_elevated {
        return Err("upgrade refuses to run as root or administrator; add --allow-elevated".into());
    }
    let dir = match dir {
        Some(d) => d,
        None => default_bin_dir()?,
    };
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this binary: {e}"))?;
    let installed = dir.join(binary_name());
    if !installed.is_file() {
        return Err(format!(
            "no installation in {}: use `install` for the first installation",
            dir.display()
        ));
    }
    if super::upgrade_fetch::is_installed(&exe, &installed) {
        return super::upgrade_fetch::fetch_and_relaunch(
            layout,
            &args,
            &crate::release::UreqFetch,
            &crate::release::trusted_keys(),
            &super::upgrade_fetch::spawn_inherited,
        );
    }
    if args.version.is_some() || args.offline.is_some() {
        return Err(
            "--version and --offline are for the installed binary: this binary replaces the installed one as it is"
                .into(),
        );
    }
    let old = installed_version(&installed);
    let running = running_roles(layout).await;
    let jobs = activity(layout).await;
    let others = pool(layout).await;
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    let roles = IpcRoles { layout, principal };
    let start = unit_starter(installed.clone());
    for line in restart_env_warnings(
        layout,
        &running.runners,
        |name| {
            crate::os_service::autostart::unit_for_argv(&[
                "runner".into(),
                "run".into(),
                "--name".into(),
                name.to_string(),
            ])
            .is_some_and(|unit| {
                crate::os_service::home_dir()
                    .is_ok_and(|h| h.join(".config/systemd/user").join(unit).is_file())
            })
        },
        |var| std::env::var(var).ok(),
    ) {
        eprintln!("warning: {line}");
    }
    let system: &dyn System = if dry_run { &DryRun } else { &OsSystem };
    let mut stdio = Stdio::new();
    let mut console = stdio.console();
    let job = Job {
        layout,
        dir: &dir,
        exe: &exe,
        old: old.as_ref(),
        new: &parse_version(&format!(
            "{} (protocol {})",
            env!("CARGO_PKG_VERSION"),
            protocol()
        ))
        .ok_or("this binary has no version")?,
        running: &running,
        activity: &jobs,
        pool: &others,
        yes,
        cancel,
        dry_run,
        wait_timeout,
    };
    job.run(&roles, system, &start, &mut console, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

fn protocol() -> String {
    let p = lr_proto::PROTOCOL_VERSION;
    format!("{}.{}", p.major, p.minor)
}

/// One `upgrade`: what is installed, what runs, and where the new binary comes from.
pub struct Job<'a> {
    /// The installation whose roles are restarted.
    pub layout: &'a Layout,
    /// The directory of the installed binary.
    pub dir: &'a Path,
    /// The new binary (this process).
    pub exe: &'a Path,
    /// The installed version, when it printed one.
    pub old: Option<&'a Version>,
    /// The version of the new binary.
    pub new: &'a Version,
    /// The roles that run now.
    pub running: &'a RunningRoles,
    /// The jobs that run and wait now.
    pub activity: &'a Activity,
    /// The runners of other installations in the pool.
    pub pool: &'a [PoolRunner],
    /// Confirm without asking.
    pub yes: bool,
    /// With running jobs: cancel them instead of waiting.
    pub cancel: bool,
    /// Print the plan and change nothing.
    pub dry_run: bool,
    /// With running jobs: stop waiting after this long; nothing is replaced then.
    pub wait_timeout: Option<Duration>,
}

impl Job<'_> {
    /// Stops the roles, replaces the binary through `system` and starts the roles again
    /// through `start`; the exit code is 1 when a role did not come back.
    pub async fn run(
        &self,
        roles: &dyn Roles,
        system: &dyn System,
        start: Spawn<'_>,
        console: &mut Console<'_>,
        interrupt: impl Future<Output = ()>,
    ) -> Result<i32, String> {
        let old_text = self.old.map_or("unknown", |v| v.text.as_str());
        let mut versions = Card::new();
        versions.row("installed", old_text);
        versions.row("new", self.new.text.clone());
        for line in versions.lines(&Out::detect()) {
            println!("{line}");
        }
        let protocol_changed = match verdict(self.old, self.new) {
            Verdict::UpToDate => {
                println!("already up to date");
                return Ok(0);
            }
            Verdict::Downgrade => {
                return Err("the new binary is older than the installed one: database schemas do not go back; nothing changed".into());
            }
            Verdict::Upgrade { protocol_changed } => protocol_changed,
        };
        if protocol_changed {
            println!(
                "warning: the major protocol version changes: upgrade the broker, the hubs and the runners of other machines in the same window"
            );
            if self.layout.broker_is_service() {
                println!(
                    "the broker service keeps answering on the old protocol until `localrunner broker update-service` (administrator rights): this command does not update it, and the new hub and runners cannot use it before that"
                );
            }
        }
        let service = self.layout.broker_is_service();
        let stop = stop_names(self.running, service);
        let mut services = std::collections::BTreeSet::new();
        let mut names: std::collections::BTreeSet<String> =
            self.layout.runner_names().into_iter().collect();
        names.extend(self.running.runners.iter().cloned());
        for name in names {
            if roles.runner_service(&name).await.is_some() {
                services.insert(name);
            }
        }
        let mut again = restart_roles(self.running, service);
        if self.running.runners.iter().all(|r| services.contains(r)) {
            again.retain(|r| *r != RoleArg::Runner);
        }
        if self.running.broker && service {
            println!(
                "the broker runs as an OS service with its own copy of the binary: this command does not touch it; update it afterwards with `localrunner broker update-service`, which needs administrator rights (see README, \"Update\")"
            );
        }
        for name in &services {
            roles.say(&crate::os_service::runner_service::upgrade_line(
                crate::os_service::Os::current(),
                name,
            ));
        }
        println!("directory: {}", self.dir.display());
        for line in self.activity.lines() {
            println!("{line}");
        }
        for line in plan_lines(self.pool, &self.new.text) {
            println!("{line}");
        }
        match stop.is_empty() {
            true => println!("no role of this installation runs"),
            false => println!(
                "stops: {}; starts again: {}",
                stop.join(", "),
                restart_summary(self.running, service)
            ),
        }
        let copies = upgrade_plan(self.exe.to_path_buf(), self.dir.to_path_buf());
        println!("{KEEP_OLD_NOTE}");
        if self.dry_run {
            for a in &copies {
                println!("{a}");
            }
            return Ok(0);
        }
        let question = format!("upgrade {old_text} to {}?", self.new.text);
        let Some(mode) = choose(
            self.activity,
            &self.running.runners,
            self.yes,
            self.cancel,
            &question,
            console,
        )?
        else {
            return Ok(1);
        };
        if mode == Mode::Cancel {
            for line in self.activity.cancel_lines(&self.running.runners) {
                roles.say(&line);
            }
        }
        let _lock = write(
            self.layout,
            &UpgradeLock::of_this_process(&self.new.text, super::timefmt::now()),
        )?;
        if !self.stop_roles(roles, mode, interrupt).await? {
            if !self.running.runners.is_empty() {
                let _ = up_with(self.layout, Some(RoleArg::Runner), STARTED_WITHIN, start).await;
            }
            return Ok(1);
        }
        let replaced = replace_binary(system, &copies).await;
        let mut code = 0;
        for role in again {
            code = code.max(
                up_with_services(self.layout, Some(role), STARTED_WITHIN, start, false)
                    .await
                    .unwrap_or(1),
            );
        }
        replaced?;
        let restarted = self.reconnects(roles, &services).await;
        let mut summary = Report::new();
        summary.line(
            Level::Ok,
            format!(
                "upgraded to {}; check the node: localrunner doctor",
                self.new.text
            ),
        );
        for line in report_lines(self.pool, &restarted, self.new, protocol_changed) {
            summary.line(Level::Warn, line);
        }
        for line in summary.lines(&Out::detect()) {
            roles.say(&line);
        }
        Ok(code)
    }

    /// Waits up to [`RECONNECT_WITHIN`] for the runners started again to be online on the new
    /// version, asking every [`RECONNECT_POLL`]; a failed answer while the hub restarts counts as
    /// not back yet, and a question still unanswered at the deadline ends the wait with what
    /// the last answer said.
    async fn reconnects(
        &self,
        roles: &dyn Roles,
        services: &std::collections::BTreeSet<String>,
    ) -> Vec<(String, Reconnected)> {
        let names: Vec<String> = self
            .running
            .runners
            .iter()
            .filter(|n| !services.contains(*n))
            .cloned()
            .collect();
        if names.is_empty() {
            return Vec::new();
        }
        let until = tokio::time::Instant::now() + RECONNECT_WITHIN;
        let mut now = Vec::new();
        loop {
            let answer = tokio::time::timeout_at(until, roles.pool_after_restart()).await;
            let late = answer.is_err();
            if let Ok(listed) = answer {
                now = listed;
            }
            let seen: Vec<(String, Reconnected)> = names
                .iter()
                .map(|n| (n.clone(), reconnected(&now, n, &self.new.text)))
                .collect();
            if late
                || seen.iter().all(|(_, back)| *back == Reconnected::Yes)
                || tokio::time::Instant::now() >= until
            {
                return seen;
            }
            tokio::time::sleep(RECONNECT_POLL).await;
        }
    }

    /// Stops the runners (draining or cancelling), waits for them, and only then the hub and a
    /// broker started by hand, so nothing but the runners is down while jobs finish. `false`
    /// when the wait ended early: the hub and the broker are untouched and nothing is replaced.
    async fn stop_roles(
        &self,
        roles: &dyn Roles,
        mode: Mode,
        interrupt: impl Future<Output = ()>,
    ) -> Result<bool, String> {
        let cancel = mode == Mode::Cancel;
        let runners: Vec<String> = self.running.runners.iter().cloned().collect();
        if !runners.is_empty() {
            let told = down_for(
                self.layout,
                runners.clone(),
                cancel,
                Some(DrainReason::Upgrade),
                roles,
                std::future::pending::<()>(),
            )
            .await?;
            if told != 0 {
                return Err("a runner could not be told to stop: nothing replaced".into());
            }
            let watching = mode == Mode::Wait && self.activity.busy();
            let started = tokio::time::Instant::now();
            let (progress, first) = WaitProgress::start(self.activity);
            if watching {
                for line in first {
                    println!("{line}");
                }
            }
            let shown = Mutex::new((progress, started + JOBS_REFRESH));
            let (names, shown) = (&runners, &shown);
            let any_answers = move || async move {
                let mut any = false;
                for n in names {
                    if roles.runner_answers(n).await {
                        any = true;
                        let _ = roles
                            .stop_runner(n, down_mode(cancel), Some(DrainReason::Upgrade))
                            .await;
                    }
                }
                if any && watching {
                    refresh_jobs(roles, shown, started).await;
                }
                any
            };
            match wait_for_runners(any_answers, cancel, self.wait_timeout, interrupt).await {
                Waited::Gone => {}
                Waited::WaitTimedOut => {
                    eprintln!(
                        "waiting stopped by --wait-timeout: nothing replaced; {} keep draining and exit when their jobs finish; the hub and the broker keep running; run `upgrade` again later",
                        runners.join(", ")
                    );
                    return Ok(false);
                }
                Waited::TimedOut => {
                    eprintln!(
                        "{} did not exit within 30 s after the cancel; the hub and the broker keep running, nothing replaced",
                        runners.join(", ")
                    );
                    return Ok(false);
                }
                Waited::Interrupted => {
                    eprintln!(
                        "stopped waiting: {} keep draining and exit when their jobs finish; the hub and the broker keep running, nothing replaced; run `upgrade` again later",
                        runners.join(", ")
                    );
                    return Ok(false);
                }
            }
        }
        let rest: Vec<String> = stop_names(self.running, self.layout.broker_is_service())
            .into_iter()
            .filter(|n| !runners.contains(n))
            .collect();
        if !rest.is_empty()
            && down_for(
                self.layout,
                rest,
                false,
                None,
                roles,
                std::future::pending::<()>(),
            )
            .await?
                != 0
        {
            return Err("the hub or the broker did not stop: nothing replaced; start them with `localrunner up`".into());
        }
        Ok(true)
    }
}

/// Asks for the running jobs every [`JOBS_REFRESH`] and prints the progress line when they
/// changed or a minute passed.
async fn refresh_jobs(
    roles: &dyn Roles,
    shown: &Mutex<(WaitProgress, tokio::time::Instant)>,
    started: tokio::time::Instant,
) {
    let now = tokio::time::Instant::now();
    if now < shown.lock().unwrap().1 {
        return;
    }
    let jobs = roles.running_job_list().await;
    let mut shown = shown.lock().unwrap();
    shown.1 = now + JOBS_REFRESH;
    let Some(jobs) = jobs else { return };
    if let Some(line) = shown.0.observe(&jobs, now - started) {
        println!("{line}");
    }
}
