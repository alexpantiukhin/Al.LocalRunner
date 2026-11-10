use super::{Action, System, Tolerate};
use std::path::Path;
use std::process::{Command, Stdio};

/// Carries out `actions` in order and stops at the first failure.
pub fn apply_all(system: &dyn System, actions: &[Action]) -> Result<(), String> {
    for a in actions {
        system.apply(a).map_err(|e| format!("{a}: {e}"))?;
    }
    Ok(())
}

/// Prints each action instead of carrying it out (`--dry-run`).
pub struct DryRun;

impl System for DryRun {
    fn apply(&self, action: &Action) -> Result<(), String> {
        println!("{action}");
        Ok(())
    }
}

/// Carries out actions on this machine.
pub struct OsSystem;

fn io(path: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", path.display())
}

fn run(program: &str, args: &[String], tolerate: &Tolerate) -> Result<(), String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"));
    let out = match (out, tolerate) {
        (Err(_), Tolerate::Anything) => return Ok(()),
        (out, _) => out?,
    };
    let code = out.status.code().unwrap_or(-1);
    let fine = match tolerate {
        Tolerate::Nothing => code == 0,
        Tolerate::Codes(c) => code == 0 || c.contains(&code),
        Tolerate::Anything => true,
    };
    if fine {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let text = String::from_utf8_lossy(&out.stdout);
        Err(format!("exit code {code}: {}{}", err.trim(), text.trim()))
    }
}

/// `policy.toml` text with `[[local_user]] name = user` added, or `None` when the user is
/// listed already (names compare case-insensitively on Windows).
pub fn ensure_local_user(text: &str, user: &str) -> Result<Option<String>, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| e.message().to_string())?;
    let listed = table
        .get("local_user")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|e| e.get("name").and_then(|n| n.as_str()))
        .any(|n| {
            if cfg!(windows) {
                n.eq_ignore_ascii_case(user)
            } else {
                n == user
            }
        });
    if listed {
        return Ok(None);
    }
    let mut out = text.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!(
        "\n[[local_user]]\nname = {}\n",
        toml::Value::String(user.to_string())
    ));
    Ok(Some(out))
}

fn add_to_path(dir: &Path) -> Result<(), String> {
    let text = dir.to_string_lossy().into_owned();
    if cfg!(windows) {
        let quoted = text.replace('\'', "''");
        let script = format!(
            "$p=[Environment]::GetEnvironmentVariable('Path','User'); if ($null -eq $p) {{ $p='' }}; \
             if (($p -split ';') -notcontains '{quoted}') {{ \
             [Environment]::SetEnvironmentVariable('Path', (($p.TrimEnd(';') + ';{quoted}').TrimStart(';')), 'User') }}"
        );
        let args = ["-NoProfile", "-NonInteractive", "-Command", &script].map(String::from);
        return run("powershell", &args, &Tolerate::Nothing);
    }
    let on_path =
        std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d == dir));
    if on_path {
        return Ok(());
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let profile = Path::new(&home).join(".profile");
    let line = format!("export PATH=\"{text}:$PATH\" # al-localrunner\n");
    let current = std::fs::read_to_string(&profile).unwrap_or_default();
    if current.contains(line.trim_end()) {
        return Ok(());
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&profile)
        .map_err(|e| io(&profile, e))?;
    f.write_all(line.as_bytes()).map_err(|e| io(&profile, e))
}

fn dscl_ids(kind: &str, attr: &str) -> Result<Vec<u32>, String> {
    let out = Command::new("dscl")
        .args([".", "-list", kind, attr])
        .output()
        .map_err(|e| format!("dscl: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().last()?.parse().ok())
        .collect())
}

fn ensure_mac_account(name: &str, home: &Path) -> Result<(), String> {
    let user = format!("/Users/{name}");
    let group = format!("/Groups/{name}");
    let exists = |path: &str| {
        Command::new("dscl")
            .args([".", "-read", path])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    if exists(&user) {
        return Ok(());
    }
    let taken: Vec<u32> = [
        dscl_ids("/Users", "UniqueID")?,
        dscl_ids("/Groups", "PrimaryGroupID")?,
    ]
    .concat();
    let id = (200..500)
        .find(|i| !taken.contains(i))
        .ok_or("no free system id between 200 and 500")?
        .to_string();
    let home = home.to_string_lossy().into_owned();
    let mut steps: Vec<Vec<&str>> = Vec::new();
    if !exists(&group) {
        steps.push(vec![".", "-create", &group]);
        steps.push(vec![".", "-create", &group, "PrimaryGroupID", &id]);
    }
    steps.extend([
        vec![".", "-create", &user],
        vec![".", "-create", &user, "UniqueID", &id],
        vec![".", "-create", &user, "PrimaryGroupID", &id],
        vec![".", "-create", &user, "UserShell", "/usr/bin/false"],
        vec![".", "-create", &user, "NFSHomeDirectory", &home],
        vec![".", "-create", &user, "Password", "*"],
        vec![".", "-create", &user, "IsHidden", "1"],
    ]);
    for s in steps {
        let args: Vec<String> = s.iter().map(|a| a.to_string()).collect();
        run("dscl", &args, &Tolerate::Nothing)?;
    }
    Ok(())
}

impl System for OsSystem {
    fn apply(&self, action: &Action) -> Result<(), String> {
        match action {
            Action::Run {
                program,
                args,
                tolerate,
            } => run(program, args, tolerate),
            Action::CreateDir(p) => std::fs::create_dir_all(p).map_err(|e| io(p, e)),
            Action::WriteFile { path, contents } => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
                }
                std::fs::write(path, contents).map_err(|e| io(path, e))
            }
            Action::CopyIfMissing { from, to } => {
                if to.exists() || !from.exists() {
                    return Ok(());
                }
                std::fs::copy(from, to).map(|_| ()).map_err(|e| io(to, e))
            }
            Action::Copy { from, to } => {
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
                }
                std::fs::copy(from, to).map(|_| ()).map_err(|e| io(to, e))
            }
            Action::RemoveFile(p) => match std::fs::remove_file(p) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io(p, e)),
                _ => Ok(()),
            },
            Action::RemoveDirAll(p) => match std::fs::remove_dir_all(p) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io(p, e)),
                _ => Ok(()),
            },
            Action::EnsureLocalUser { policy, user } => {
                let text = match std::fs::read_to_string(policy) {
                    Ok(t) => t,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(e) => return Err(io(policy, e)),
                };
                match ensure_local_user(&text, user)? {
                    Some(new) => std::fs::write(policy, new).map_err(|e| io(policy, e)),
                    None => Ok(()),
                }
            }
            Action::AddToUserPath(dir) => add_to_path(dir),
            Action::EnsureMacAccount { name, home } => ensure_mac_account(name, home),
        }
    }
}
