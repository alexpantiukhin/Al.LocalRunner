//! `config set|unset|get|show`: one setting of the file a role reads.

use crate::config_edit::{
    plan, resolve, run_change_with, ApplyOpts, Change, ConfigDoc, Elevator, LiveRoles, OsElevator,
    RoleControl, Target,
};
use crate::paths::{service_root, Layout};
use std::io::Write;

/// What `config set` or `config unset` does to the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Sets `key` to `value`; `string` stores the text as a string.
    Set {
        /// The dotted key.
        key: String,
        /// The value as TOML text.
        value: String,
        /// Store the text as a string.
        string: bool,
    },
    /// Removes `key`.
    Unset {
        /// The dotted key.
        key: String,
    },
}

/// One `config set` or `config unset` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditArgs {
    /// The file, named by its role.
    pub target: Target,
    /// The edit.
    pub edit: Edit,
    /// Print the change and write nothing.
    pub dry_run: bool,
    /// Restart the role when the key applies only at its next start.
    pub restart: bool,
}

/// The command-line word of `target`.
pub fn target_word(target: &Target) -> String {
    match target {
        Target::Broker => "broker".into(),
        Target::Hub => "hub".into(),
        Target::Runner(name) => format!("runner:{name}"),
        Target::Logging => "logging".into(),
    }
}

/// The directories the elevated re-run of a command that writes `change` is given
/// explicitly (the elevated process starts with a fresh environment): for the broker service's
/// file the service's own directories, else those of `layout`.
pub fn elevated_dirs(layout: &Layout, change: &Change) -> Vec<String> {
    let service = service_root();
    let (root, config) = if change.path.starts_with(&service) {
        (service.clone(), service)
    } else {
        (layout.root.clone(), layout.config.clone())
    };
    vec![
        "--state-dir".to_string(),
        root.display().to_string(),
        "--config-dir".to_string(),
        config.display().to_string(),
    ]
}

/// The arguments of the elevated re-run of `args`: [`elevated_dirs`], then the same command
/// without `--dry-run`.
pub fn child_command(layout: &Layout, args: &EditArgs, change: &Change) -> Vec<String> {
    let mut out = elevated_dirs(layout, change);
    out.push("config".to_string());
    match &args.edit {
        Edit::Set { key, value, string } => {
            out.extend([
                "set".into(),
                target_word(&args.target),
                key.clone(),
                value.clone(),
            ]);
            if *string {
                out.push("--string".into());
            }
        }
        Edit::Unset { key } => {
            out.extend(["unset".into(), target_word(&args.target), key.clone()]);
        }
    }
    if args.restart {
        out.push("--restart".into());
    }
    out
}

/// Reads the file of `args.target`, applies the edit and validates it; nothing is written. An
/// unset of a key the file does not have is an error.
pub fn plan_edit(layout: &Layout, args: &EditArgs) -> Result<Change, String> {
    let path = resolve(layout, &args.target)?;
    plan(layout, &args.target, |doc| match &args.edit {
        Edit::Set { key, value, string } => doc.set(key, value, *string),
        Edit::Unset { key } => match doc.unset(key)? {
            true => Ok(()),
            false => Err(format!("`{key}` is not set in {}", path.display())),
        },
    })
}

/// `config set` or `config unset` on this machine, with the elevation of this OS.
pub async fn edit(layout: &Layout, args: EditArgs, elevated_child: bool) -> Result<i32, String> {
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    edit_with(
        layout,
        args,
        elevated_child,
        &OsElevator,
        &LiveRoles,
        &mut out,
        &mut err,
    )
    .await
}

/// `config set` or `config unset` with the given elevation, roles and output.
pub async fn edit_with(
    layout: &Layout,
    args: EditArgs,
    elevated_child: bool,
    elevator: &dyn Elevator,
    roles: &dyn RoleControl,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, String> {
    let change = plan_edit(layout, &args)?;
    let opts = ApplyOpts {
        dry_run: args.dry_run,
        restart: args.restart,
        elevated_child,
        args: child_command(layout, &args, &change),
        keys: vec![match &args.edit {
            Edit::Set { key, .. } | Edit::Unset { key } => key.clone(),
        }],
    };
    run_change_with(
        layout,
        &args.target,
        change,
        opts,
        elevator,
        roles,
        out,
        err,
    )
    .await
}

fn read(layout: &Layout, target: &Target) -> Result<(std::path::PathBuf, Option<String>), String> {
    let path = resolve(layout, target)?;
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok((path, Some(text))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((path, None)),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// `config get`: the TOML text of `key` in the file of `target`; a table is shown as TOML. A
/// key the file does not set is an error that names the file.
pub fn get(layout: &Layout, target: &Target, key: &str) -> Result<String, String> {
    let (path, text) = read(layout, target)?;
    let text = text.unwrap_or_default();
    if let Some(value) = ConfigDoc::parse(&text)?.get(key) {
        return Ok(value);
    }
    let table = text
        .parse::<toml::Table>()
        .map_err(|e| format!("invalid TOML in {}: {e}", path.display()))?;
    let mut parts = key.split('.');
    let found = parts
        .next()
        .and_then(|first| table.get(first))
        .and_then(|first| parts.try_fold(first, |v, part| v.as_table()?.get(part)));
    match found {
        Some(toml::Value::Table(t)) => toml::to_string(t).map_err(|e| e.to_string()),
        Some(v) => Ok(v.to_string()),
        None => Err(format!(
            "`{key}` is not set in {}: the role uses its default",
            path.display()
        )),
    }
}

/// `config show`: the path of the file of `target` and its text.
pub fn show(layout: &Layout, target: &Target) -> Result<String, String> {
    let (path, text) = read(layout, target)?;
    Ok(match text {
        Some(text) => format!("# {}\n{text}", path.display()),
        None => format!(
            "# {}\n# the file does not exist: every setting has its default",
            path.display()
        ),
    })
}
