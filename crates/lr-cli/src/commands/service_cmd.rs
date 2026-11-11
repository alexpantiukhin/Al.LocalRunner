//! `broker install-service` and `autostart on|off [role]` (spec 3.12, 10, 11.1). Everything the
//! OS does goes through a plan of [`crate::os_service::Action`]s; `--dry-run` prints it.

use super::{RoleArg, Switch};
use crate::init::{self, NodeRole};
use crate::os_service::plans::{self, AutostartInputs, Entry, ServiceInputs};
use crate::os_service::{
    apply_all, apply_or_undo, current_uid, home_dir, original_user, service_binary,
    service_installed, Action, DryRun, Os, OsSystem, System,
};
use crate::paths::{service_root, Layout};

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

/// `autostart on|off [role [runner-name]] [--dry-run]`: the broker becomes or stops being the
/// OS service, hubs and runners start at the user's logon; `runner <name>` is one runner.
pub fn autostart(
    layout: &Layout,
    switch: Switch,
    role: Option<RoleArg>,
    runner: Option<String>,
    dry_run: bool,
) -> Result<i32, String> {
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
    if let Some(name) = runner {
        if role != Some(RoleArg::Runner) {
            return Err(
                "a name only goes with the role `runner`: autostart on runner <name>".into(),
            );
        }
        crate::runner_config::check_runner_name(&name)?;
        let entry = Entry::Runner(Some(name));
        let label = entry.name();
        apply(&autostart_plan(layout, entry, on)?, dry_run).map_err(task_hint)?;
        if !dry_run {
            println!("autostart {}: {label}", if on { "on" } else { "off" });
        }
        return Ok(0);
    }
    for role in chosen_roles(layout, role)? {
        if role == NodeRole::Broker {
            if on {
                install_service_with(layout, dry_run, elevated)?;
            } else {
                needs_admin("autostart off broker", dry_run, elevated)?;
                apply(&uninstall_service_plan(), dry_run)?;
            }
            continue;
        }
        for entry in entries(layout, role) {
            let name = entry.name();
            apply(&autostart_plan(layout, entry, on)?, dry_run).map_err(task_hint)?;
            if !dry_run {
                let state = if on { "on" } else { "off" };
                println!("autostart {state}: {name}");
            }
        }
    }
    if on && !dry_run && Os::current() == Os::Linux {
        println!("to start without logging in: loginctl enable-linger");
    }
    Ok(0)
}
