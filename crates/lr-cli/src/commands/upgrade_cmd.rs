//! `upgrade [--dir] [--yes] [--dry-run]` (spec 10.5): run by the NEW binary, it replaces the
//! installed one. It tells the installed and the new version, stops the roles of this
//! installation that run (runners drain, then the hub and a broker started by hand), copies
//! itself over the installed binary and starts the same roles again from it. The broker OS
//! service keeps its own copy of the binary and is left alone.

use super::args::{RoleArg, UpgradeArgs};
use super::confirm::{Console, Stdio};
use super::down_cmd::{down_with, wait_for_runners, IpcRoles, Roles, Waited};
use super::setup::{binary_name, default_bin_dir, install_plan};
use super::up_cmd::{running_roles, starter, up_with, RunningRoles, Spawn};
use super::upgrade_jobs::{activity, choose, Activity, Mode};
use crate::os_service::{apply_all, Action, DryRun, OsSystem, System};
use crate::paths::Layout;
use std::future::Future;
use std::path::Path;
use std::time::Duration;

const STARTED_WITHIN: Duration = Duration::from_secs(15);
const BUSY_FOR: Duration = Duration::from_secs(20);

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

/// Applies the copies; a stopped role may hold its binary for a few seconds more, so a failure
/// is retried for [`BUSY_FOR`] before it is reported.
async fn replace_binary(system: &dyn System, copies: &[Action]) -> Result<(), String> {
    let start = tokio::time::Instant::now();
    loop {
        match apply_all(system, copies) {
            Err(e) if start.elapsed() >= BUSY_FOR => return Err(e),
            Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
            Ok(()) => return Ok(()),
        }
    }
}

fn installed_version(path: &Path) -> Option<Version> {
    let out = std::process::Command::new(path)
        .arg("--version")
        .output()
        .ok()?;
    parse_version(&String::from_utf8_lossy(&out.stdout))
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
    } = args;
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
    if let (Ok(a), Ok(b)) = (exe.canonicalize(), installed.canonicalize()) {
        if a == b {
            return Err(format!(
                "this is the installed binary ({}): run the new binary from another folder",
                installed.display()
            ));
        }
    }
    let old = installed_version(&installed);
    let running = running_roles(layout).await;
    let jobs = activity(layout).await;
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    let roles = IpcRoles { layout, principal };
    let start = starter(installed.clone());
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
        yes,
        cancel,
        dry_run,
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
    /// Confirm without asking.
    pub yes: bool,
    /// With running jobs: cancel them instead of waiting.
    pub cancel: bool,
    /// Print the plan and change nothing.
    pub dry_run: bool,
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
        println!("installed: {old_text}\nnew:       {}", self.new.text);
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
        }
        let service = self.layout.broker_is_service();
        let stop = stop_names(self.running, service);
        let again = restart_roles(self.running, service);
        if self.running.broker && service {
            println!(
                "the broker runs as an OS service with its own copy of the binary: this command does not touch it; update it afterwards with `localrunner broker update-service`, which needs administrator rights (see README, \"Update\")"
            );
        }
        println!("directory: {}", self.dir.display());
        for line in self.activity.lines() {
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
        let copies: Vec<Action> = install_plan(self.exe.to_path_buf(), self.dir.to_path_buf())
            .into_iter()
            .filter(|a| matches!(a, Action::Copy { .. }))
            .collect();
        if self.dry_run {
            for a in &copies {
                println!("{a}");
            }
            return Ok(0);
        }
        let question = format!("upgrade {old_text} to {}?", self.new.text);
        let Some(mode) = choose(self.activity, self.yes, self.cancel, &question, console)? else {
            return Ok(1);
        };
        if !self.stop_roles(roles, mode, interrupt).await? {
            return Ok(1);
        }
        let replaced = replace_binary(system, &copies).await;
        let mut code = 0;
        for role in again {
            code = code.max(
                up_with(self.layout, Some(role), STARTED_WITHIN, start)
                    .await
                    .unwrap_or(1),
            );
        }
        replaced?;
        println!(
            "upgraded to {}; check the node: localrunner doctor",
            self.new.text
        );
        Ok(code)
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
            let told = down_with(
                self.layout,
                runners.clone(),
                cancel,
                roles,
                std::future::pending::<()>(),
            )
            .await?;
            if told != 0 {
                return Err("a runner could not be told to stop: nothing replaced".into());
            }
            if mode == Mode::Wait && self.activity.busy() {
                eprintln!("waiting for the running jobs to finish (Ctrl+C stops waiting)");
            }
            let names = &runners;
            let any_answers = move || async move {
                for n in names {
                    if roles.runner_answers(n).await {
                        return true;
                    }
                }
                false
            };
            match wait_for_runners(any_answers, cancel, interrupt).await {
                Waited::Gone => {}
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
            && down_with(
                self.layout,
                rest,
                false,
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
