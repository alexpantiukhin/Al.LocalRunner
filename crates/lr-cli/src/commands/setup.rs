//! `init`, `install` and `uninstall` (spec 10.1, 10.2, 10.5, 11.1).

use super::service_cmd::{autostart_plan, entries, uninstall_service_plan};
use super::RoleArg;
use crate::init::{self, NodeRole};
use crate::os_service::autostart::WINDOWS_LAUNCHER;
use crate::os_service::plans::Entry;
use crate::os_service::{
    apply_all, original_user, service_installed, Action, DryRun, Os, OsSystem, System,
};
use crate::paths::Layout;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

fn roles_of(role: RoleArg) -> Vec<NodeRole> {
    match role {
        RoleArg::Hub => vec![NodeRole::Hub],
        RoleArg::Broker => vec![NodeRole::Broker],
        RoleArg::Runner => vec![NodeRole::Runner],
        RoleArg::All => NodeRole::ALL.to_vec(),
    }
}

/// `init [--role ...]`.
pub fn init(layout: &Layout, role: RoleArg) -> Result<i32, String> {
    let roles = roles_of(role);
    for c in init::init(layout, &roles, &original_user()?)? {
        let what = if c.written { "created" } else { "kept   " };
        println!("{what} {}", c.path.display());
    }
    println!("next: localrunner doctor");
    Ok(0)
}

/// Where `install` puts the binary by default (spec 10.3).
pub fn default_bin_dir() -> Result<PathBuf, String> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(|d| PathBuf::from(d).join("Programs").join("Al.LocalRunner"))
            .ok_or_else(|| "LOCALAPPDATA is not set".into())
    } else {
        Ok(crate::os_service::home_dir()?.join(".local").join("bin"))
    }
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "localrunner.exe"
    } else {
        "localrunner"
    }
}

fn binaries() -> Vec<&'static str> {
    let mut out = vec![binary_name()];
    if cfg!(windows) {
        out.push(WINDOWS_LAUNCHER);
    }
    out
}

/// The plan of `install`: the binary (on Windows with its windowless launcher) into `dir`,
/// `dir` on the user's `PATH`.
pub fn install_plan(exe: PathBuf, dir: PathBuf) -> Vec<Action> {
    let mut out = vec![Action::CreateDir(dir.clone())];
    for name in binaries() {
        let from = exe.with_file_name(name);
        let to = dir.join(name);
        if from != to {
            out.push(Action::Copy { from, to });
        }
    }
    out.push(Action::AddToUserPath(dir));
    out
}

fn system(dry_run: bool) -> &'static dyn System {
    if dry_run {
        &DryRun
    } else {
        &OsSystem
    }
}

/// `install [--dir] [--role] [--allow-elevated] [--dry-run]`: idempotent, as an ordinary user;
/// it starts no role and enables no autostart (spec 10.1).
pub fn install(
    layout: &Layout,
    dir: Option<PathBuf>,
    role: RoleArg,
    allow_elevated: bool,
    dry_run: bool,
) -> Result<i32, String> {
    if crate::host::is_elevated() && !allow_elevated {
        return Err("install refuses to run as root or administrator; add --allow-elevated".into());
    }
    let dir = match dir {
        Some(d) => d,
        None => default_bin_dir()?,
    };
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this binary: {e}"))?;
    apply_all(system(dry_run), &install_plan(exe, dir.clone()))?;
    if dry_run {
        println!("init --role {role:?} in {}", layout.root.display());
        return Ok(0);
    }
    init(layout, role)?;
    println!(
        "installed in {}; open a new terminal for PATH, then: up, broker add or hub join",
        dir.display()
    );
    Ok(0)
}

/// Whether `uninstall --purge` may delete the state directory of `layout`: `--yes`, or `yes`
/// typed at the prompt written to `out`; without a terminal only `--yes` confirms.
pub fn purge_confirmed(
    layout: &Layout,
    yes: bool,
    terminal: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<bool, String> {
    if yes {
        return Ok(true);
    }
    if !terminal {
        return Err("--purge deletes the state directory; confirm with --yes".into());
    }
    let _ = write!(
        out,
        "delete {} with keys, databases and logs? type yes: ",
        layout.root.display()
    );
    let _ = out.flush();
    let mut line = String::new();
    input.read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(line.trim() == "yes")
}

fn confirmed(layout: &Layout, yes: bool) -> Result<bool, String> {
    let terminal = std::io::stdin().is_terminal();
    purge_confirmed(
        layout,
        yes,
        terminal,
        &mut std::io::stdin().lock(),
        &mut std::io::stdout(),
    )
}

/// The plan of `uninstall` without stopping roles: autostart entries off, the broker service
/// removed when `with_service`, the binary of `dir` removed, and the state directory with
/// `purge`.
pub fn uninstall_plan(
    layout: &Layout,
    dir: PathBuf,
    with_service: bool,
    purge: bool,
) -> Result<Vec<Action>, String> {
    let mut out = Vec::new();
    let mut all = entries(layout, NodeRole::Runner);
    all.push(Entry::Hub);
    if !all.contains(&Entry::Runner(None)) {
        all.push(Entry::Runner(None));
    }
    for e in all {
        out.extend(autostart_plan(layout, e, false)?);
    }
    if with_service {
        out.extend(uninstall_service_plan());
    }
    for name in binaries() {
        out.push(Action::RemoveFile(dir.join(name)));
    }
    out.push(Action::RemoveFromUserPath(dir));
    if purge {
        out.push(Action::RemoveDirAll(layout.root.clone()));
    }
    Ok(out)
}

/// Whether `uninstall` goes on after `down` answered `stopped`: only when every role stopped
/// (or did not run), so the binary is never removed under a running hub, broker or runner.
pub fn uninstall_may_go_on(stopped: Result<i32, String>) -> Result<(), String> {
    let hint = "nothing was removed; stop the roles with `localrunner down` and run \
                `localrunner uninstall` again";
    match stopped {
        Ok(0) => Ok(()),
        Ok(_) => Err(format!("some roles did not stop: {hint}")),
        Err(e) => Err(format!("stopping the roles: {e}; {hint}")),
    }
}

/// `uninstall [--purge] [--yes] [--dir] [--dry-run]` (spec 10.5): stops the roles (`down`), removes
/// autostart, the broker service (as administrator) and the binary; keys and history stay
/// unless `--purge` is confirmed. Keys stay registered at brokers until `runner remove`.
pub async fn uninstall(
    layout: &Layout,
    purge: bool,
    yes: bool,
    dir: Option<PathBuf>,
    dry_run: bool,
) -> Result<i32, String> {
    let dir = match dir {
        Some(d) => d,
        None => default_bin_dir()?,
    };
    let mut code = 0;
    if !dry_run {
        let stopped = super::down_cmd::down(layout, Vec::new(), true).await;
        if let Err(why) = uninstall_may_go_on(stopped) {
            eprintln!("{why}");
            return Ok(1);
        }
    }
    let service = service_installed(Os::current());
    let with_service = service && (dry_run || crate::host::is_elevated());
    if service && !with_service {
        eprintln!(
            "the broker service stays: run `localrunner autostart off broker` as administrator"
        );
        code = 1;
    }
    let purge = purge && (dry_run || confirmed(layout, yes)?);
    let plan = uninstall_plan(layout, dir, with_service, purge)?;
    for a in &plan {
        if let Err(e) = system(dry_run).apply(a) {
            eprintln!("{a}: {e}");
            code = 1;
        }
    }
    if !dry_run {
        println!("uninstalled");
    }
    Ok(code)
}
