//! `secret set|list|remove`: the secrets of a runner without touching its `runner.toml` by hand.
//! A value lives in a private file of the state directory and `[secret.NAME] from = "file:..."`
//! points at it, so it survives restarts, `upgrade` and OS services; the value is never printed.

use super::config_cmd::{edit_with, Edit, EditArgs};
use crate::config_edit::{resolve, Elevator, RoleControl, Target};
use crate::paths::Layout;
use crate::runner_config::{secret_sources, SecretFrom};
use crate::secrets::ConfigSecrets;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Where `secret set` takes the value from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueSource {
    /// Standard input, up to the end of the stream.
    Stdin,
    /// A file with the value.
    File(PathBuf),
}

/// One `secret set` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetArgs {
    /// The runner whose `runner.toml` gets the secret.
    pub runner: String,
    /// The secret name.
    pub name: String,
    /// Where the value comes from.
    pub source: ValueSource,
    /// Show the change and write nothing.
    pub dry_run: bool,
    /// Restart the role when the key applies only at its next start.
    pub restart: bool,
}

/// Whether `name` can be a secret name and a file name: letters, digits, `-` and `_`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The file that holds the value of secret `name` of runner `runner`.
pub fn secret_file(layout: &Layout, runner: &str, name: &str) -> PathBuf {
    layout.root.join("secrets").join(runner).join(name)
}

fn read_value(source: &ValueSource, stdin: &mut dyn Read) -> Result<String, String> {
    let mut bytes = Vec::new();
    match source {
        ValueSource::Stdin => stdin
            .read_to_end(&mut bytes)
            .map_err(|e| format!("standard input: {e}"))?,
        ValueSource::File(path) => std::fs::File::open(path)
            .and_then(|mut f| f.read_to_end(&mut bytes))
            .map_err(|e| format!("{}: {e}", path.display()))?,
    };
    let text = String::from_utf8(bytes).map_err(|_| "the value is not UTF-8 text".to_string())?;
    let value = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(&text)
        .to_string();
    if value.is_empty() {
        return Err("the value is empty: nothing was written".into());
    }
    Ok(value)
}

fn write_private(path: &Path, value: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("no directory for the secret file")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let private = std::fs::Permissions::from_mode(0o700);
        let _ = std::fs::set_permissions(dir, private);
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    file.write_all(value.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// `secret set`: checks the name and the value, writes the private file, then sets
/// `secret.NAME.from` in the runner's file with the diff, backup and reload of `config set`.
#[allow(clippy::too_many_arguments)]
pub async fn set_with(
    layout: &Layout,
    args: SetArgs,
    elevated_child: bool,
    elevator: &dyn Elevator,
    roles: &dyn RoleControl,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, String> {
    if !valid_name(&args.name) {
        return Err(format!(
            "secret name {:?}: use letters, digits, - and _ only",
            args.name
        ));
    }
    let value = read_value(&args.source, stdin)?;
    let path = secret_file(layout, &args.runner, &args.name);
    let existed = path.exists();
    let from = format!("file:{}", path.display());
    if args.dry_run {
        let _ = writeln!(
            out,
            "would write the value of {} to {} (private to its owner)",
            args.name,
            path.display()
        );
    } else {
        write_private(&path, &value)?;
    }
    let edit = EditArgs {
        target: Target::Runner(args.runner.clone()),
        edit: Edit::Set {
            key: format!("secret.{}.from", args.name),
            value: from,
            string: true,
        },
        dry_run: args.dry_run,
        restart: args.restart,
        expected_before: None,
    };
    let result = edit_with(layout, edit, elevated_child, elevator, roles, out, err).await;
    if !matches!(result, Ok(0)) && !args.dry_run && !existed {
        let _ = std::fs::remove_file(&path);
    }
    result
}

/// `secret remove`: takes `secret.NAME` out of the runner's file and deletes the value file
/// this command made for it.
#[allow(clippy::too_many_arguments)]
pub async fn remove_with(
    layout: &Layout,
    runner: &str,
    name: &str,
    dry_run: bool,
    restart: bool,
    elevated_child: bool,
    elevator: &dyn Elevator,
    roles: &dyn RoleControl,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, String> {
    let own = secret_file(layout, runner, name);
    let edit = EditArgs {
        target: Target::Runner(runner.to_string()),
        edit: Edit::Unset {
            key: format!("secret.{name}"),
        },
        dry_run,
        restart,
        expected_before: None,
    };
    let result = edit_with(layout, edit, elevated_child, elevator, roles, out, err).await;
    if matches!(result, Ok(0)) && !dry_run && own.is_file() {
        std::fs::remove_file(&own).map_err(|e| format!("{}: {e}", own.display()))?;
    }
    result
}

fn describe(from: &SecretFrom) -> String {
    match from {
        SecretFrom::Env(v) => format!("env:{v}"),
        SecretFrom::File(p) => format!("file:{}", p.display()),
        SecretFrom::Keychain(e) => format!("keychain:{e}"),
    }
}

/// `secret list`: one line per secret of the runner (all runners of the installation without a
/// name) with its source and whether the source gives a value now; never a value.
pub async fn list_lines(layout: &Layout, runner: Option<&str>) -> Result<Vec<String>, String> {
    let runners = match runner {
        Some(r) => vec![r.to_string()],
        None => match layout.runner_names() {
            names if names.is_empty() => vec!["default".to_string()],
            names => names,
        },
    };
    let mut lines = Vec::new();
    for name in runners {
        let path = resolve(layout, &Target::Runner(name.clone()))?;
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let table: toml::Table = text
            .parse()
            .map_err(|e| format!("invalid TOML in {}: {e}", path.display()))?;
        let sources = secret_sources(&table);
        let env = crate::unit_env::runner_env(layout, &name).await;
        let unavailable = ConfigSecrets::new(sources.clone())
            .unavailable_in(&|v| env.lookup(v), &|v| env.missing(v));
        for (secret, from) in &sources {
            let state = unavailable
                .iter()
                .find(|(n, _)| n == secret)
                .map(|(_, why)| format!("not available: {why}"))
                .unwrap_or_else(|| "available".to_string());
            lines.push(format!("{name}  {secret}  {}  {state}", describe(from)));
        }
    }
    if lines.is_empty() {
        lines.push("(no secrets)".into());
    }
    Ok(lines)
}
