//! `broker install-service` and `autostart on|off [role]` (spec 3.12, 10, 11.1). Everything the
//! OS does goes through a plan of [`crate::os_service::Action`]s; `--dry-run` prints it.

use super::{RoleArg, Switch};
use crate::init::{self, NodeRole};
use crate::os_service::plans::{self, AutostartInputs, Entry, ServiceInputs};
use crate::os_service::runner_service::{self, RunnerServiceInputs};
use crate::os_service::state::{named_restart_command, named_status_command};
use crate::os_service::{
    apply_all, apply_or_undo, current_uid, home_dir, original_user, service_binary,
    service_installed, Action, DryRun, Os, OsSystem, System,
};
use crate::paths::{service_root, Layout};
use std::path::Path;
use std::time::{Duration, Instant};

fn apply(actions: &[Action], dry_run: bool) -> Result<(), String> {
    let system: &dyn System = if dry_run { &DryRun } else { &OsSystem };
    apply_all(system, actions)
}

fn needs_admin(what: &str, dry_run: bool, elevated: bool) -> Result<(), String> {
    if dry_run || elevated {
        Ok(())
    } else {
        Err(format!(
            "{what} needs administrator rights (run it elevated or with sudo)"
        ))
    }
}

/// The one command that runs `broker update-service` with administrator rights on `os`; it is
/// printed for the user and never run by this binary.
pub fn elevated_command(os: Os, exe: &Path) -> String {
    let exe = exe.display().to_string();
    match os {
        Os::Windows => {
            let exe = exe.replace('\'', "\'\'");
            format!(
                "$e = '{exe}'; $o = Join-Path $env:TEMP 'localrunner-update-service.txt'; \
                 Start-Process -Verb RunAs -Wait -FilePath cmd.exe \
                 -ArgumentList \"/d /c `\"`\"$e`\" broker update-service > `\"$o`\" 2>&1`\"\"; \
                 Get-Content $o"
            )
        }
        _ => format!("sudo {exe} broker update-service"),
    }
}

fn admin_marker(dry_run: bool, elevated: bool) {
    if dry_run {
        let rights = if elevated {
            "this shell is elevated"
        } else {
            "this shell is not elevated: a real run stops before them"
        };
        println!("# the steps below need administrator rights ({rights})");
    }
}

/// The line `autostart` prints for the part it left to an administrator.
pub fn admin_left(on: bool) -> String {
    let (verb, how) = if cfg!(windows) {
        ("autostart", "in a terminal started as administrator")
    } else {
        ("autostart", "with sudo")
    };
    format!(
        "left for an administrator: the broker service. Run `localrunner {verb} {} broker` {how}",
        if on { "on" } else { "off" }
    )
}

fn exe() -> Result<std::path::PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("cannot find this binary: {e}"))
}

/// The plan of `broker install-service` for this machine.
pub fn install_service_plan(layout: &Layout) -> Result<Vec<Action>, String> {
    let os = Os::current();
    Ok(plans::install_service(&ServiceInputs {
        os,
        exe: exe()?,
        binary: service_binary(os),
        root: service_root(),
        configs: layout.config.clone(),
        user: original_user()?,
    }))
}

/// `broker install-service [--dry-run]`.
pub fn install_service(layout: &Layout, dry_run: bool) -> Result<i32, String> {
    install_service_with(layout, dry_run, crate::host::is_elevated())
}

/// `broker install-service` in a process whose rights are `elevated`: without them and without
/// `--dry-run` it is refused before any change (spec 16.5).
pub fn install_service_with(layout: &Layout, dry_run: bool, elevated: bool) -> Result<i32, String> {
    needs_admin("broker install-service", dry_run, elevated)?;
    let plan = install_service_plan(layout)?;
    if dry_run {
        admin_marker(true, elevated);
        apply(&plan, true)?;
    } else {
        let os = Os::current();
        let undo = plans::install_service_undo(os, service_installed(os));
        apply_or_undo(&OsSystem, &plan, &undo)?;
    }
    if !dry_run {
        println!(
            "the broker runs as a service from {}; `localrunner broker add` prints join codes",
            service_root().display()
        );
        if Os::current() == Os::Linux {
            println!("log in again so that your user's new group membership applies");
        }
    }
    Ok(0)
}

/// `broker update-service [--dry-run]`.
pub fn update_service(dry_run: bool) -> Result<i32, String> {
    update_service_with(
        dry_run,
        crate::host::is_elevated(),
        service_installed(Os::current()),
    )
}

/// `broker update-service` in a process whose rights are `elevated`, on a machine where the
/// service `installed` or not: it replaces the service's own copy of the binary with this one
/// and restarts the service. Without administrator rights it is refused before any change.
pub fn update_service_with(dry_run: bool, elevated: bool, installed: bool) -> Result<i32, String> {
    let os = Os::current();
    let this = exe()?;
    if !dry_run && !elevated {
        return Err(format!(
            "broker update-service needs administrator rights; run: {}",
            elevated_command(os, &this)
        ));
    }
    if !installed {
        return Err(
            "the broker is not installed as a service: run `broker install-service`".into(),
        );
    }
    let new = env!("CARGO_PKG_VERSION");
    let binary = service_binary(os);
    let read = |path: &Path| super::upgrade_cmd::installed_version(path).map(|v| v.text);
    admin_marker(dry_run, elevated);
    if dry_run {
        println!("{}", version_line(read(&binary).as_deref(), new));
        return apply(&plans::update_service(os, &this, &binary), true).map(|()| 0);
    }
    let report = update_service_on(&OsSystem, os, &this, &binary, &read, new)?;
    for line in report_lines(&report) {
        println!("{line}");
    }
    Ok(if report.running_new { 0 } else { 1 })
}

/// What `broker update-service` did to the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateReport {
    /// The version of the service binary before, when it printed one.
    pub old: Option<String>,
    /// The version installed.
    pub new: String,
    /// From the stop of the service to its start.
    pub down: Duration,
    /// Whether the service binary reports the new version after the start.
    pub running_new: bool,
}

/// `service: <old> -> <new>`.
pub fn version_line(old: Option<&str>, new: &str) -> String {
    format!("service: {} -> {new}", old.unwrap_or("unknown"))
}

/// The lines printed after an update.
pub fn report_lines(report: &UpdateReport) -> Vec<String> {
    let mut out = vec![
        version_line(report.old.as_deref(), &report.new),
        format!(
            "service stopped and started, down for {} s",
            report.down.as_secs()
        ),
    ];
    if !report.running_new {
        out.push(format!(
            "the service binary still runs another version than {}: check `localrunner doctor`",
            report.new
        ));
    }
    out
}

/// Stops the service, replaces its binary and starts it again through `system`, timing the
/// stop; `read` gives the version of a binary, asked before the plan and after the start.
pub fn update_service_on(
    system: &dyn System,
    os: Os,
    exe: &Path,
    binary: &Path,
    read: &dyn Fn(&Path) -> Option<String>,
    new: &str,
) -> Result<UpdateReport, String> {
    let old = read(binary);
    let plan = plans::update_service(os, exe, binary);
    let (mut stopped, mut down) = (None, Duration::ZERO);
    for (i, action) in plan.iter().enumerate() {
        system.apply(action).map_err(|e| format!("{action}: {e}"))?;
        if i == 0 {
            stopped = Some(Instant::now());
        }
        if i + 1 == plan.len() {
            down = stopped.map_or(Duration::ZERO, |t| t.elapsed());
        }
    }
    let running_new = read(binary).as_deref() == Some(new);
    Ok(UpdateReport {
        old,
        new: new.to_string(),
        down,
        running_new,
    })
}

/// The plan that removes the broker service.
pub fn uninstall_service_plan() -> Vec<Action> {
    let os = Os::current();
    plans::uninstall_service(os, &service_binary(os))
}

/// The roles `role` names; without one, the roles `init` created.
pub fn chosen_roles(layout: &Layout, role: Option<RoleArg>) -> Result<Vec<NodeRole>, String> {
    let roles = match role {
        Some(RoleArg::Hub) => vec![NodeRole::Hub],
        Some(RoleArg::Broker) => vec![NodeRole::Broker],
        Some(RoleArg::Runner) => vec![NodeRole::Runner],
        Some(RoleArg::All) => NodeRole::ALL.to_vec(),
        None => init::roles(layout),
    };
    if roles.is_empty() {
        return Err("name a role (hub, broker, runner) or run `localrunner init` first".into());
    }
    Ok(roles)
}

/// The autostart entries of a hub or runner role: one per runner of the installation, or the
/// default runner when there is none yet.
pub fn entries(layout: &Layout, role: NodeRole) -> Vec<Entry> {
    match role {
        NodeRole::Hub => vec![Entry::Hub],
        NodeRole::Broker => Vec::new(),
        NodeRole::Runner => {
            let names = layout.runner_names();
            if names.is_empty() {
                vec![Entry::Runner(None)]
            } else {
                names.into_iter().map(|n| Entry::Runner(Some(n))).collect()
            }
        }
    }
}

/// The autostart plan of `entry` for this user.
pub fn autostart_plan(layout: &Layout, entry: Entry, on: bool) -> Result<Vec<Action>, String> {
    let exe = exe()?;
    let launcher = exe.with_file_name(crate::os_service::autostart::WINDOWS_LAUNCHER);
    if on && Os::current() == Os::Windows && !launcher.is_file() {
        return Err(format!(
            "{} is missing next to this binary; install both files again",
            launcher.display()
        ));
    }
    let inputs = AutostartInputs {
        os: Os::current(),
        exe,
        entry,
        state_dir: layout.root.clone(),
        config_dir: (layout.config != layout.root).then(|| layout.config.clone()),
        home: home_dir()?,
        uid: current_uid(),
        user: lr_net::current_os_user().map_err(|e| e.to_string())?,
        principal: lr_net::current_os_principal().map_err(|e| e.to_string())?,
    };
    Ok(if on {
        plans::autostart_on(&inputs)
    } else {
        plans::autostart_off(&inputs)
    })
}

fn task_hint(e: String) -> String {
    if Os::current() != Os::Windows {
        return e;
    }
    format!(
        "{e}\nTask Scheduler did not take the task. Run the command as the user the role is for, \
         in that user's session; where the machine's policy lets only administrators create \
         tasks, run it from an elevated prompt of that same user"
    )
}

fn runner_may_start(layout: &Layout, name: &str) -> Result<(), String> {
    let os = Os::current();
    if let Some(why) =
        runner_service::service_conflict(os, name, runner_service::installed(os, name))
    {
        return Err(why);
    }
    let allowed = crate::doctor::runner_allows_same_user(layout, name);
    match crate::doctor::same_user_verdict(layout, allowed) {
        Ok(Some(risk)) => {
            eprintln!("runner {name}: warning: {risk}");
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(refusal) => Err(format!("autostart runner {name}: {refusal}")),
    }
}

/// `autostart on|off [role [runner-name]] [--service] [--dry-run]`: the broker becomes or stops
/// being the OS service, hubs and runners start at the user's logon; `runner <name>` is one
/// runner, and with `service` that runner becomes or stops being an OS service
/// ([`runner_service_with`]). A runner the same-user rule of spec 3.12 refuses gets no
/// autostart entry.
pub fn autostart(
    layout: &Layout,
    switch: Switch,
    role: Option<RoleArg>,
    runner: Option<String>,
    service: bool,
    dry_run: bool,
) -> Result<i32, String> {
    if service {
        let verb = if switch == Switch::On { "on" } else { "off" };
        let usage =
            format!("--service is for one runner: autostart {verb} runner <name> --service");
        return match (role, runner) {
            (Some(RoleArg::Runner), Some(name)) => {
                runner_service_with(layout, switch, &name, dry_run, crate::host::is_elevated())
            }
            _ => Err(usage),
        };
    }
    autostart_with(
        layout,
        switch,
        role,
        runner,
        dry_run,
        crate::host::is_elevated(),
    )
}

/// `autostart` in a process whose rights are `elevated`: the broker service needs them unless
/// `--dry-run` (spec 16.5).
pub fn autostart_with(
    layout: &Layout,
    switch: Switch,
    role: Option<RoleArg>,
    runner: Option<String>,
    dry_run: bool,
    elevated: bool,
) -> Result<i32, String> {
    let on = switch == Switch::On;
    let requested = role;
    if let Some(name) = runner {
        if role != Some(RoleArg::Runner) {
            return Err(
                "a name only goes with the role `runner`: autostart on runner <name>".into(),
            );
        }
        crate::runner_config::check_runner_name(&name)?;
        if on {
            runner_may_start(layout, &name)?;
        }
        let entry = Entry::Runner(Some(name));
        let label = entry.name();
        apply(&autostart_plan(layout, entry, on)?, dry_run).map_err(task_hint)?;
        if !dry_run {
            println!("{}", autostart_line(on, &label));
        }
        return Ok(0);
    }
    let mut left_for_admin = false;
    for role in chosen_roles(layout, role)? {
        if role == NodeRole::Broker {
            if broker_left_for_admin(requested, dry_run, elevated) {
                left_for_admin = true;
                continue;
            }
            if on {
                install_service_with(layout, dry_run, elevated)?;
            } else {
                needs_admin("autostart off broker", dry_run, elevated)?;
                admin_marker(dry_run, elevated);
                apply(&uninstall_service_plan(), dry_run)?;
            }
            continue;
        }
        for entry in entries(layout, role) {
            if let (true, Entry::Runner(runner)) = (on, &entry) {
                let name = runner
                    .clone()
                    .unwrap_or_else(|| crate::roles::runner_base_name(layout, None, None));
                let os = Os::current();
                if runner_service::installed(os, &name) {
                    println!(
                        "runner {name}: skipped, it runs as the OS service {}",
                        runner_service::service_name(os, &name)
                    );
                    continue;
                }
                runner_may_start(layout, &name)?;
            }
            let name = entry.name();
            apply(&autostart_plan(layout, entry, on)?, dry_run).map_err(task_hint)?;
            if !dry_run {
                println!("{}", autostart_line(on, &name));
            }
        }
    }
    if let Some(hint) = linger_hint(Os::current(), on, dry_run) {
        println!("{hint}");
    }
    if left_for_admin {
        println!("{}", admin_left(on));
    }
    Ok(0)
}

/// The line `autostart on|off` prints for a login entry: `on` says when it starts.
pub fn autostart_line(on: bool, entry: &str) -> String {
    if on {
        format!("autostart on: {entry} (starts at user login)")
    } else {
        format!("autostart off: {entry}")
    }
}

fn runner_needs(layout: &Layout, name: &str) -> Vec<String> {
    let Some(text) = crate::build::runner_config_path(layout, name, None)
        .and_then(|p| std::fs::read_to_string(p).ok())
    else {
        return Vec::new();
    };
    let Ok(table) = toml::from_str::<toml::Table>(&text) else {
        return Vec::new();
    };
    let words = |key: &str| -> Vec<String> {
        table
            .get(key)
            .and_then(toml::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let services = table
        .get("service")
        .and_then(toml::Value::as_table)
        .map(|t| t.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    words("tags")
        .into_iter()
        .chain(words("platforms"))
        .chain(services)
        .collect()
}

fn shell_safe(word: &str) -> String {
    let safe = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+:@%,=".contains(c));
    if safe {
        word.to_string()
    } else {
        crate::os_service::quote::shell_word(word)
    }
}

/// The one command that runs `autostart on|off runner <name> --service` with administrator
/// rights on `os` for the installation of `layout` (its directories are named, so the elevated
/// run does not fall back to the administrator's own); printed, never run by this binary.
pub fn runner_service_command(
    os: Os,
    exe: &Path,
    switch: Switch,
    name: &str,
    layout: &Layout,
) -> String {
    let verb = if switch == Switch::On { "on" } else { "off" };
    let mut dirs = vec![("--state-dir", &layout.root)];
    if layout.config != layout.root {
        dirs.push(("--config-dir", &layout.config));
    }
    let tail = format!("autostart {verb} runner {name} --service");
    match os {
        Os::Windows => {
            let q = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "''"));
            let dirs: String = dirs
                .iter()
                .map(|(flag, p)| format!(" {flag} {}", q(p)))
                .collect();
            format!("& {} {tail}{dirs}", q(exe))
        }
        _ => {
            let dirs: String = dirs
                .iter()
                .map(|(flag, p)| format!(" {flag} {}", shell_safe(&p.display().to_string())))
                .collect();
            format!(
                "sudo {} {tail}{dirs}",
                shell_safe(&exe.display().to_string())
            )
        }
    }
}

/// `autostart on|off runner <name> --service` in a process whose rights are `elevated`: the
/// runner becomes or stops being an OS service ([`runner_service`]). `on` refuses the `ci`
/// runner, a runner that needs a desktop session, a runner the same-user rule of spec 3.12
/// refuses, and root or `SYSTEM` as its identity. Without administrator rights and without
/// `--dry-run` it is refused before any change, with the exact command to run.
pub fn runner_service_with(
    layout: &Layout,
    switch: Switch,
    name: &str,
    dry_run: bool,
    elevated: bool,
) -> Result<i32, String> {
    let user = original_user()?;
    let os = Os::current();
    let login = |n: &str| runner_service::login_entry(os, &user, n);
    let service = |n: &str| runner_service::service_config(os, n);
    let poll = |n: &str, polls: usize, settle: usize, want: bool| {
        runner_service::confirm_state(
            &mut || runner_service::running(os, n),
            &mut || std::thread::sleep(Duration::from_secs(1)),
            polls,
            settle,
            want,
        )
    };
    let started = |n: &str| poll(n, 15, 3, true);
    let stopped = |n: &str| poll(n, 30, 1, false).is_ok();
    let ctx = ServiceContext {
        user: user.clone(),
        elevated,
        principal: &runner_service::principal,
        access: &runner_service::key_state,
        login_entry: &login,
        service: &service,
        confirm_started: &started,
        confirm_stopped: &stopped,
    };
    runner_service_for(layout, switch, name, dry_run, &ctx)
}

/// What [`runner_service_for`] learns from the machine: the installing user (under `sudo` the
/// one who called it, elevated on Windows the session user), the rights of this process, the
/// id of a user, the ownership of a path, whether a login entry of that user starts a runner,
/// and the installed service of a runner.
pub struct ServiceContext<'a> {
    /// The installing OS user: the runner's owner and, on Linux and macOS, its account.
    pub user: String,
    /// Whether this process has administrator rights.
    pub elevated: bool,
    /// The id of an OS user: a SID on Windows, a uid elsewhere.
    pub principal: &'a dyn Fn(&str) -> Option<String>,
    /// The ownership of a path as this process sees it.
    pub access: &'a dyn Fn(&Path) -> runner_service::KeyState,
    /// Whether a login entry of [`ServiceContext::user`] starts the named runner.
    pub login_entry: &'a dyn Fn(&str) -> bool,
    /// The installed service of the named runner as the service manager holds it: `None`
    /// without a service, `Some(None)` when it cannot be read (always on Linux and macOS).
    pub service: &'a dyn Fn(&str) -> Option<Option<runner_service::ServiceConfig>>,
    /// Whether the started service of the named runner keeps running: `Err` with its last
    /// state otherwise (bounded).
    pub confirm_started: &'a dyn Fn(&str) -> Result<(), Option<bool>>,
    /// Whether the service of the named runner stopped (bounded).
    pub confirm_stopped: &'a dyn Fn(&str) -> bool,
}

/// [`runner_service_with`] for `ctx`. `on` refuses, in this order: root or `SYSTEM` as the
/// user (by name or id); the `ci` runner and a runner that needs a desktop session; a runner
/// missing from the installation or whose directory the user does not own; on Windows a runner
/// without its own broker key, or whose key is not the user's (a re-run over the user's own
/// service excepted); a runner a login entry also starts; a runner the same-user rule of spec
/// 3.12 refuses for the installing user. Then the rights. `off` on Windows gives the key back to
/// the user the service was installed for, and is refused when that user cannot be told;
/// otherwise only without the rights.
pub fn runner_service_for(
    layout: &Layout,
    switch: Switch,
    name: &str,
    dry_run: bool,
    ctx: &ServiceContext<'_>,
) -> Result<i32, String> {
    let on = switch == Switch::On;
    let verb = if on { "on" } else { "off" };
    let elevated = ctx.elevated;
    crate::runner_config::check_runner_name(name)?;
    let os = Os::current();
    let exe = exe()?;
    let mut inputs = RunnerServiceInputs {
        os,
        exe: exe.clone(),
        name: name.to_string(),
        state_dir: layout.root.clone(),
        config_dir: (layout.config != layout.root).then(|| layout.config.clone()),
        user: ctx.user.clone(),
    };
    let key = runner_service::key_path(&inputs);
    let account = runner_service::account(&inputs);
    let user_id = (ctx.principal)(&ctx.user);
    let service_id = (ctx.principal)(&account);
    let installed = (ctx.service)(name);
    let owner = installed.as_ref().map(|c| {
        c.as_ref()
            .and_then(|c| runner_service::owner_from_command_line(&c.command))
    });
    if os == Os::Windows {
        let service = runner_service::service_name(os, name);
        let theirs = match &installed {
            Some(Some(config)) => runner_service::not_ours(name, config, &inputs),
            Some(None) => Some(format!(
                "cannot read the configuration of the service {service}, so it is left alone; \
                 check it as administrator: {}",
                named_status_command(os, &service)
            )),
            None => None,
        };
        if let Some(why) = theirs {
            return Err(format!("autostart {verb} runner {name} --service: {why}"));
        }
    }
    let mut restore_key = false;
    if on {
        let refused = |why: String| format!("autostart on runner {name} --service: {why}");
        if let Some(why) = runner_service::identity_refusal(&ctx.user, user_id.as_deref()) {
            return Err(refused(why));
        }
        let dir = layout.runner_dir(name);
        let joined = dir.join(crate::build::BROKER_FILE).is_file();
        let why = runner_service::refusal(name, &runner_needs(layout, name))
            .or_else(|| {
                runner_service::runner_dir_refusal(
                    name,
                    dir.is_dir(),
                    dir.is_dir()
                        && runner_service::dir_owned(os, &(ctx.access)(&dir), user_id.as_deref()),
                    &ctx.user,
                )
            })
            .or_else(|| runner_service::local_runner_refusal(os, name, joined))
            .or_else(|| {
                (os == Os::Windows)
                    .then(|| {
                        runner_service::key_handover_refusal(
                            name,
                            &ctx.user,
                            user_id.as_deref(),
                            service_id.as_deref(),
                            &key,
                            &(ctx.access)(&key),
                            owner.clone(),
                        )
                    })
                    .flatten()
            })
            .or_else(|| {
                runner_service::login_entry_refusal(name, (ctx.login_entry)(name), &ctx.user)
            });
        if let Some(why) = why {
            return Err(refused(why));
        }
        let allowed = crate::doctor::runner_allows_same_user(layout, name);
        if let Some(risk) = crate::doctor::same_user_verdict_for(layout, allowed, Some(&ctx.user))
            .map_err(|e| format!("autostart runner {name}: {e}"))?
        {
            eprintln!("runner {name}: warning: {risk}");
        }
    } else if os == Os::Windows {
        let exists = (ctx.access)(&key) != runner_service::KeyState::Missing;
        if let Some(owner) = runner_service::restore_owner(name, &key, exists, owner.clone())
            .map_err(|e| format!("autostart off runner {name} --service: {e}"))?
        {
            inputs.user = owner;
            restore_key = true;
        }
    }
    if !dry_run && !elevated {
        let how = if os == Os::Windows {
            "run it in a terminal started as administrator"
        } else {
            "run"
        };
        return Err(format!(
            "autostart {verb} runner {name} --service needs administrator rights; {how}: {}",
            runner_service_command(os, &exe, switch, name, layout)
        ));
    }
    admin_marker(dry_run, elevated);
    let system: &dyn System = if dry_run { &DryRun } else { &OsSystem };
    if on {
        runner_service::apply_install(system, &inputs, installed.is_some())?;
        if os == Os::Windows && !dry_run {
            runner_service::handover_verdict(
                name,
                &key,
                &(ctx.access)(&key),
                &account,
                (ctx.principal)(&account).as_deref(),
                &ctx.user,
            )?;
        }
        if !dry_run {
            (ctx.confirm_started)(name).map_err(|state| {
                runner_service::start_failure(
                    os,
                    name,
                    state,
                    &layout.root.join("logs").join("service.log"),
                )
            })?;
        }
    } else {
        runner_service::apply_uninstall(system, &inputs, restore_key, &mut || {
            dry_run || (ctx.confirm_stopped)(name)
        })?;
        if restore_key && !dry_run {
            let owner_id = (ctx.principal)(&inputs.user);
            if let Some(note) = runner_service::restore_verdict(
                name,
                &key,
                &(ctx.access)(&key),
                &inputs.user,
                owner_id.as_deref(),
                &account,
            )? {
                println!("{note}");
            }
        }
    }
    let existed = on && installed.is_some();
    if dry_run {
        return Ok(0);
    }
    let service = runner_service::service_name(os, name);
    if on {
        println!(
            "autostart on: runner {name} started as the OS service {service} under {} and kept \
             running for a few seconds; it starts after a reboot and keeps running after logout; \
             whether it reached its broker: localrunner status",
            runner_service::account(&inputs)
        );
        println!("its state: {}", named_status_command(os, &service));
        if existed {
            println!(
                "the service existed already: restart it so the running runner takes this \
                 definition: {}",
                named_restart_command(os, &service)
            );
        }
        if os == Os::Windows {
            println!(
                "the service account does not have your PATH or the tools installed for your \
                 user only: jobs see only tools installed for all users"
            );
        }
        println!(
            "stopping the service ends running jobs at once, without a drain; use `localrunner \
             down {name}` first where it reaches the runner"
        );
    } else {
        println!("autostart off: runner {name} is no longer an OS service");
    }
    Ok(0)
}

/// Whether `autostart` leaves the broker service to an administrator: the role was not named
/// (`all` or the roles of `init`), so the user part still goes ahead, and the shell cannot do it.
pub fn broker_left_for_admin(requested: Option<RoleArg>, dry_run: bool, elevated: bool) -> bool {
    !dry_run && !elevated && requested != Some(RoleArg::Broker)
}

/// The hint `autostart on` prints on Linux once the user units are in place: without
/// `loginctl enable-linger` they start only when the user logs in.
pub fn linger_hint(os: Os, on: bool, dry_run: bool) -> Option<&'static str> {
    (on && !dry_run && os == Os::Linux)
        .then_some("to start without logging in: loginctl enable-linger")
}
