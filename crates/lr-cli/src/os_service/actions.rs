use super::programs::program_path;
use super::quote::shell_word;
use super::trust::{require_trusted, secure_service_dir};
use super::{service_account, Action, Os, System, Tolerate};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Carries out `actions` in order and stops at the first failure.
pub fn apply_all(system: &dyn System, actions: &[Action]) -> Result<(), String> {
    for a in actions {
        system.apply(a).map_err(|e| format!("{a}: {e}"))?;
    }
    Ok(())
}

/// Carries out `plan`; when an action fails, carries out `undo` as far as it goes and returns
/// the failure.
pub fn apply_or_undo(system: &dyn System, plan: &[Action], undo: &[Action]) -> Result<(), String> {
    let Err(e) = apply_all(system, plan) else {
        return Ok(());
    };
    if undo.is_empty() {
        return Err(e);
    }
    for a in undo {
        let _ = system.apply(a);
    }
    Err(format!(
        "{e} (undone: the service this run registered was removed)"
    ))
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
    let exe = match (program_path(program), tolerate) {
        (Ok(p), _) => p,
        (Err(_), Tolerate::Anything) => return Ok(()),
        (Err(e), _) => return Err(e),
    };
    let out = Command::new(&exe)
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

/// The Windows PowerShell script that adds `dir` to (or, with `add` false, removes it from)
/// the user's `Path`, keeping the value's `%VARIABLE%` references (it is read unexpanded and
/// written as `REG_EXPAND_SZ`), then tells running programs the environment changed.
pub fn path_script(dir: &Path, add: bool) -> String {
    let quoted = dir.to_string_lossy().replace('\'', "''");
    let change = if add {
        "if ($parts -notcontains $d) { $parts += $d }"
    } else {
        "$parts = @($parts | Where-Object { $_ -ne $d })"
    };
    format!(
        "$k=[Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment'); $p=$k.GetValue('Path','',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames); $d='{quoted}'; $parts=@($p -split ';' | Where-Object {{ $_ -ne '' }}); {change}; $k.SetValue('Path', ($parts -join ';'), [Microsoft.Win32.RegistryValueKind]::ExpandString); $k.Close(); [Environment]::SetEnvironmentVariable('AlLocalRunnerRefresh','1','User'); [Environment]::SetEnvironmentVariable('AlLocalRunnerRefresh',$null,'User')"
    )
}

/// The line `install` adds to shell profiles: `dir` in single quotes, so nothing in it expands.
pub fn profile_line(dir: &Path) -> String {
    format!(
        "export PATH={}:\"$PATH\" # al-localrunner\n",
        shell_word(&dir.to_string_lossy())
    )
}

/// `text` without the lines [`profile_line`] added for `dir`.
pub fn profile_without(text: &str, dir: &Path) -> String {
    let line = profile_line(dir);
    let line = line.trim_end();
    text.lines()
        .filter(|l| l.trim_end() != line)
        .map(|l| format!("{l}\n"))
        .collect()
}

fn profiles(home: &Path) -> Vec<PathBuf> {
    let mut out = vec![home.join(".profile")];
    for name in [".bash_profile", ".zprofile"] {
        let p = home.join(name);
        if p.exists() || (name == ".zprofile" && cfg!(target_os = "macos")) {
            out.push(p);
        }
    }
    out
}

fn user_path(dir: &Path, add: bool) -> Result<(), String> {
    if cfg!(windows) {
        let script = path_script(dir, add);
        let args = ["-NoProfile", "-NonInteractive", "-Command", &script].map(String::from);
        return run("powershell", &args, &Tolerate::Nothing);
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let line = profile_line(dir);
    for profile in profiles(Path::new(&home)) {
        let current = std::fs::read_to_string(&profile).unwrap_or_default();
        let next = if add {
            if current.lines().any(|l| l.trim_end() == line.trim_end()) {
                continue;
            }
            let mut next = current.clone();
            if !next.is_empty() && !next.ends_with('\n') {
                next.push('\n');
            }
            next + &line
        } else {
            if !current.contains(line.trim_end()) {
                continue;
            }
            profile_without(&current, dir)
        };
        std::fs::write(&profile, next).map_err(|e| io(&profile, e))?;
    }
    Ok(())
}

fn utf16_with_bom(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

fn dscl_ids(kind: &str, attr: &str) -> Result<Vec<u32>, String> {
    let out = Command::new(program_path("dscl")?)
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
    let dscl = program_path("dscl")?;
    let exists = |path: &str| {
        Command::new(&dscl)
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
                if std::fs::symlink_metadata(to).is_ok() {
                    return require_trusted(to, service_account(Os::current()));
                }
                if !from.exists() {
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
            Action::AddToUserPath(dir) => user_path(dir, true),
            Action::RemoveFromUserPath(dir) => user_path(dir, false),
            Action::WriteUtf16 { path, contents } => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
                }
                std::fs::write(path, utf16_with_bom(contents)).map_err(|e| io(path, e))
            }
            Action::SecureServiceDir { path, account } => secure_service_dir(path, account),
            Action::EnsureMacAccount { name, home } => ensure_mac_account(name, home),
        }
    }
}
