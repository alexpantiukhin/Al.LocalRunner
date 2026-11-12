//! `profile check`: the secrets a project profile asks for, checked before a run.

use crate::paths::Layout;
use crate::runner_config::SecretFrom;
use lr_broker::policy::PolicyFile;
use lr_core::Profile;
use lr_runner::secret_var;
use std::collections::{BTreeMap, BTreeSet};

/// What `profile check` knows about the machine.
pub struct Machine {
    /// The policy of the broker, when it can be read here.
    pub policy: Option<PolicyFile>,
    /// The client label the policy compares with.
    pub label: String,
    /// The `[secret.*]` sources of every runner, by runner name.
    pub runners: Vec<(String, BTreeMap<String, SecretFrom>)>,
    /// Whether an environment variable is set to a non-empty value in this shell.
    pub env_set: fn(&str) -> bool,
}

fn wanted(profile: &Profile) -> BTreeSet<String> {
    profile
        .secrets
        .iter()
        .chain(profile.steps.iter().flat_map(|s| s.secrets.iter()))
        .cloned()
        .collect()
}

/// The report lines for `profiles`; the flag is false when something would stop a run.
pub fn report(profiles: &[Profile], machine: &Machine) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut ok = true;
    for p in profiles {
        let names = wanted(p);
        if names.is_empty() {
            continue;
        }
        out.push(format!("profile {}", p.name));
        for name in names {
            out.push(format!(
                "  secret {name}: the job gets it as the variable {}",
                secret_var(&name)
            ));
            match &machine.policy {
                None => out.push(
                    "    policy: not checked, policy.toml is not readable here (ask the machine owner)"
                        .into(),
                ),
                Some(file) if file.secrets_allowed(&machine.label).contains(&name) => out.push(
                    format!("    policy: allowed for the label \"{}\"", machine.label),
                ),
                Some(_) => {
                    ok = false;
                    out.push(format!(
                        "    policy: NOT allowed for the label \"{}\": add it to a [[secrets.allow]] entry with this label (the label of the [[local_user]] entry, not the --as of a run)",
                        machine.label
                    ));
                }
            }
            if machine.runners.is_empty() {
                out.push("    runner: no runner of this installation to check".into());
            }
            for (runner, sources) in &machine.runners {
                match sources.get(&name) {
                    None => {
                        ok = false;
                        out.push(format!("    runner {runner}: no [secret.{name}] in its runner.toml"));
                    }
                    Some(SecretFrom::Env(var)) if (machine.env_set)(var) => out.push(format!(
                        "    runner {runner}: env:{var}, set in this shell (a runner started earlier does not see it: restart it)"
                    )),
                    Some(SecretFrom::Env(var)) => {
                        ok = false;
                        out.push(format!(
                            "    runner {runner}: env:{var} is not set in this shell; the variable must exist in the environment of the runner process"
                        ));
                    }
                    Some(SecretFrom::Keychain(entry)) => {
                        out.push(format!("    runner {runner}: keychain:{entry} (not read by this check)"));
                    }
                }
            }
        }
    }
    if out.is_empty() {
        out.push("no profile asks for a secret".into());
    }
    (out, ok)
}

fn var_set(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !v.is_empty())
}

fn os_user() -> Option<String> {
    ["USERNAME", "USER"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .filter(|v| !v.is_empty())
}

fn local_label(policy: &PolicyFile) -> Option<String> {
    let me = os_user()?.to_lowercase();
    policy
        .local_users
        .iter()
        .find(|u| u.name.to_lowercase().rsplit('\\').next() == Some(me.as_str()))
        .map(|u| u.label.clone())
}

fn read_policy(layout: &Layout) -> Option<PolicyFile> {
    let path = if layout.broker_is_service() {
        crate::paths::service_root().join("policy.toml")
    } else {
        layout.config_file("policy.toml")
    };
    lr_broker::policy::load(&std::fs::read_to_string(path).ok()?).ok()
}

fn runner_sources(layout: &Layout) -> Vec<(String, BTreeMap<String, SecretFrom>)> {
    let mut names = layout.runner_names();
    if names.is_empty() {
        names.push("(template)".into());
    }
    let host = crate::host::resources();
    names
        .into_iter()
        .filter_map(|name| {
            let path = crate::build::runner_config_path(layout, &name, None)?;
            let text = std::fs::read_to_string(path).ok()?;
            let host_name = crate::facts::host_name();
            let cfg = crate::runner_config::RunnerConfig::parse(
                &text,
                Some(name.as_str()).filter(|n| *n != "(template)"),
                &host_name,
                layout.root.clone(),
                host,
            )
            .ok()?;
            Some((name, cfg.secrets))
        })
        .collect()
}

/// `profile check [profile] [--label L]` in the current directory.
pub fn check(layout: &Layout, profile: Option<&str>, label: Option<&str>) -> Result<i32, String> {
    let path = std::path::Path::new(".localrunner").join("profiles.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut profiles = lr_core::parse_profiles(&text).map_err(|e| e.to_string())?;
    if let Some(name) = profile {
        profiles.retain(|p| p.name == name);
        if profiles.is_empty() {
            return Err(format!("no profile {name} in {}", path.display()));
        }
    }
    let policy = read_policy(layout);
    let label = label
        .map(str::to_string)
        .or_else(|| policy.as_ref().and_then(local_label))
        .unwrap_or_default();
    let machine = Machine {
        policy,
        label,
        runners: runner_sources(layout),
        env_set: var_set,
    };
    let (lines, ok) = report(&profiles, &machine);
    for l in lines {
        println!("{l}");
    }
    Ok(i32::from(!ok))
}
