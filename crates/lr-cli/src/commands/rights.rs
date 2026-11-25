//! Commands that need administrator rights ask for them themselves: an ordinary terminal is
//! enough. Only the step that needs the rights runs elevated (the UAC prompt on Windows,
//! `sudo` elsewhere), as a re-run of that one command, and its output and exit code come back
//! to the terminal the user typed in. Nothing is elevated without an interactive terminal,
//! without a yes at the question, or with `--no-elevate`.

use super::confirm::{confirm, Console, Stdio};
use crate::config_edit::{elevated_command_line, rerun_elevated, Elevator, OsElevator};
use crate::os_service::Os;
use crate::paths::Layout;
use std::io::Write;
use std::path::Path;

/// What is done with the rights and why they are needed, as the question says it.
#[derive(Debug, Clone, Copy)]
pub struct Need<'a> {
    /// The command, for example `broker update-service`.
    pub what: &'a str,
    /// What the step does with the rights.
    pub why: &'a str,
}

/// The state of the process that wants the rights.
#[derive(Debug, Clone, Copy)]
pub struct Session {
    /// This process already has them.
    pub elevated: bool,
    /// This process is the elevated re-run of a command and must not elevate again.
    pub child: bool,
    /// `--dry-run`: nothing is changed, so nothing is elevated.
    pub dry_run: bool,
    /// `--no-elevate`: never ask for rights.
    pub no_elevate: bool,
}

/// What `ask_for_rights` decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Run the step in this process: it has the rights, or nothing is changed.
    Here,
    /// The step ran elevated; this is its exit code.
    Elevated(i32),
    /// The user did not agree.
    Declined,
}

/// The session of this process.
pub fn session(dry_run: bool, no_elevate: bool, child: bool) -> Session {
    Session {
        elevated: crate::host::is_elevated(),
        child,
        dry_run,
        no_elevate,
    }
}

/// The global flags that point a re-run at this installation, before the subcommand.
pub fn layout_args(layout: &Layout) -> Vec<String> {
    vec![
        "--state-dir".into(),
        layout.root.display().to_string(),
        "--config-dir".into(),
        layout.config.display().to_string(),
    ]
}

/// Decides how the step runs. Without the rights, with a terminal and without `--no-elevate`
/// it says what will be done with them, asks, and on a yes runs `args` (the whole command
/// line after the program) elevated through `elevator`. Without a terminal, or with
/// `--no-elevate`, it is an error that names the command to run by hand. A prompt the user
/// refuses is reported by `elevator` on `err` with the command to run by hand.
#[allow(clippy::too_many_arguments)]
pub fn ask_for_rights(
    need: &Need<'_>,
    session: &Session,
    args: &[String],
    console: &mut Console<'_>,
    elevator: &dyn Elevator,
    os: Os,
    exe: &Path,
    err: &mut dyn Write,
) -> Result<Outcome, String> {
    if session.dry_run || session.elevated || session.child {
        return Ok(Outcome::Here);
    }
    if session.no_elevate || !console.terminal {
        return Err(format!(
            "{} needs administrator rights; run: {}",
            need.what,
            elevated_command_line(os, exe, args)
        ));
    }
    let _ = writeln!(
        console.out,
        "{} needs administrator rights: {}",
        need.what, need.why
    );
    let how = if os == Os::Windows {
        "a UAC prompt"
    } else {
        "sudo"
    };
    let question = format!("ask for them now ({how})?");
    if !confirm(need.what, &question, false, console)? {
        let _ = writeln!(
            console.out,
            "not run: administrator rights were not granted"
        );
        return Ok(Outcome::Declined);
    }
    Ok(Outcome::Elevated(rerun_elevated(
        elevator, os, exe, args, err,
    )))
}

/// A command that failed because the OS refused it the admin channel of the broker: with a
/// terminal it asks for the rights and runs `args` elevated, and the exit code is the re-run's.
/// Any other failure, or no terminal, `--no-elevate`, or a no, keeps the original result.
pub fn retry_if_denied(
    result: Result<i32, String>,
    need: &Need<'_>,
    session: &Session,
    args: &[String],
) -> Result<i32, String> {
    let denied = matches!(&result, Err(e)
        if e.starts_with("the broker admin channel is not reachable") && super::admin_cmd::access_denied(e));
    if !denied {
        return result;
    }
    match offer(need, session, args) {
        Some(code) => Ok(code),
        None => result,
    }
}

fn this_binary() -> Result<std::path::PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("cannot find this binary: {e}"))
}

/// Runs `run` in this process when it can, otherwise as the elevated re-run of `args`; the
/// exit code is the step's. A user who says no gets exit code 1.
pub fn run_with_rights(
    need: &Need<'_>,
    session: &Session,
    args: &[String],
    run: impl FnOnce() -> Result<i32, String>,
) -> Result<i32, String> {
    let exe = this_binary()?;
    let mut stdio = Stdio::new();
    let mut console = stdio.console();
    let mut err = std::io::stderr();
    let outcome = ask_for_rights(
        need,
        session,
        args,
        &mut console,
        &OsElevator,
        Os::current(),
        &exe,
        &mut err,
    )?;
    match outcome {
        Outcome::Here => run(),
        Outcome::Elevated(code) => Ok(code),
        Outcome::Declined => Ok(1),
    }
}

/// Offers a step that the command left for an administrator: with a terminal it asks and runs
/// `args` elevated and returns its exit code; without one, with `--no-elevate` or on a no
/// it returns `None` and the command keeps the hint it already printed.
pub fn offer(need: &Need<'_>, session: &Session, args: &[String]) -> Option<i32> {
    let exe = this_binary().ok()?;
    let mut stdio = Stdio::new();
    let mut console = stdio.console();
    let mut err = std::io::stderr();
    match ask_for_rights(
        need,
        session,
        args,
        &mut console,
        &OsElevator,
        Os::current(),
        &exe,
        &mut err,
    ) {
        Ok(Outcome::Elevated(code)) => Some(code),
        _ => None,
    }
}
