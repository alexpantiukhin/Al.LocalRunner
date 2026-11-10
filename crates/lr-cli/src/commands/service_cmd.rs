//! `broker install-service` and `autostart on|off [role]` (spec 3.12, 10, 11.1). Everything the
//! OS does goes through a plan of [`crate::os_service::Action`]s; `--dry-run` prints it.

use super::{RoleArg, Switch};
use crate::init::{self, NodeRole};
use crate::os_service::plans::{self, AutostartInputs, Entry, ServiceInputs};
use crate::os_service::{
    apply_all, current_uid, home_dir, original_user, service_binary, Action, DryRun, Os, OsSystem,
    System,
};
use crate::paths::{service_root, Layout};

fn apply(actions: &[Action], dry_run: bool) -> Result<(), String> {
    let system: &dyn System = if dry_run { &DryRun } else { &OsSystem };
    apply_all(system, actions)
}

fn needs_admin(what: &str, dry_run: bool) -> Result<(), String> {
    if dry_run || crate::host::is_elevated() {
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
    needs_admin("broker install-service", dry_run)?;
    apply(&install_service_plan(layout)?, dry_run)?;
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
    let inputs = AutostartInputs {
        os: Os::current(),
        exe: exe()?,
        entry,
        state_dir: layout.root.clone(),
        config_dir: (layout.config != layout.root).then(|| layout.config.clone()),
        home: home_dir()?,
        uid: current_uid(),
    };
    Ok(if on {
        plans::autostart_on(&inputs)
    } else {
        plans::autostart_off(&inputs)
    })
}

/// `autostart on|off [role] [--dry-run]`: the broker becomes or stops being the OS service,
/// hubs and runners start at the user's logon.
pub fn autostart(
    layout: &Layout,
    switch: Switch,
    role: Option<RoleArg>,
    dry_run: bool,
) -> Result<i32, String> {
    let on = switch == Switch::On;
    for role in chosen_roles(layout, role)? {
        if role == NodeRole::Broker {
            if on {
                install_service(layout, dry_run)?;
            } else {
                needs_admin("autostart off broker", dry_run)?;
                apply(&uninstall_service_plan(), dry_run)?;
            }
            continue;
        }
        for entry in entries(layout, role) {
            let name = entry.name();
            apply(&autostart_plan(layout, entry, on)?, dry_run)?;
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
