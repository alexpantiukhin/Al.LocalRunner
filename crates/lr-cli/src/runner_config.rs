//! `runner.toml` as the assembly reads it (spec 3.3, 3.10, 3.12, 3.13, 4): the agent's own
//! settings plus what only the assembly uses — resources, elevation, program checks and
//! secret sources.

use lr_broker::policy::HostResources;
use lr_runner::{RunnerSettings, SettingsError};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Default time a `[programs]` check may take (spec 4).
pub const DEFAULT_CHECK_TIMEOUT_S: u64 = 10;

/// One `[programs]` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramSpec {
    /// Explicit path; else the program is looked up in `PATH`.
    pub path: Option<PathBuf>,
    /// Command that must exit with 0.
    pub check: Option<String>,
    /// Seconds the check may take.
    pub check_timeout_s: Option<u64>,
}

/// Where a secret comes from (`[secret.NAME] from = "..."`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretFrom {
    /// `env:VAR`: an environment variable of the runner process.
    Env(String),
    /// `keychain:ENTRY`: the OS credential store.
    Keychain(String),
}

#[derive(Deserialize)]
struct SecretEntry {
    from: String,
}

#[derive(Deserialize, Default)]
struct Extra {
    name: Option<String>,
    mem_mb: Option<u64>,
    cpu: Option<u32>,
    #[serde(default)]
    allow_elevated: bool,
    #[serde(default)]
    programs: BTreeMap<String, ProgramSpec>,
    #[serde(default)]
    secret: BTreeMap<String, SecretEntry>,
}

/// A parsed `runner.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerConfig {
    /// What the agent works with; `programs` holds only the programs that passed their checks
    /// after [`RunnerConfig::check_programs`].
    pub settings: RunnerSettings,
    /// `allow_elevated = true`: the owner allows running as root or administrator.
    pub allow_elevated: bool,
    /// `[programs]` as written.
    pub programs: BTreeMap<String, ProgramSpec>,
    /// `[secret.*]` sources.
    pub secrets: BTreeMap<String, SecretFrom>,
}

fn secret_from(name: &str, text: &str) -> Result<SecretFrom, SettingsError> {
    match text.split_once(':') {
        Some(("env", var)) if !var.is_empty() => Ok(SecretFrom::Env(var.to_string())),
        Some(("keychain", entry)) if !entry.is_empty() => {
            Ok(SecretFrom::Keychain(entry.to_string()))
        }
        _ => Err(SettingsError(format!(
            "[secret.{name}] from = \"{text}\": use env:VARIABLE or keychain:ENTRY"
        ))),
    }
}

impl RunnerConfig {
    /// Reads `text`. The name is `name` when given (`--name`), else `name` of the file, else
    /// `default_name` (the host name). Without `mem_mb` or `cpu` the runner offers all of
    /// `host`: one runner runs one job at a time and gives it everything it declares. `PATH`
    /// of jobs comes only from `[env] path`; the `PATH` of the runner process is not inherited.
    pub fn parse(
        text: &str,
        name: Option<&str>,
        default_name: &str,
        state_dir: PathBuf,
        host: HostResources,
    ) -> Result<RunnerConfig, SettingsError> {
        let mut table: toml::Table =
            toml::from_str(text).map_err(|e| SettingsError(e.message().to_string()))?;
        let extra: Extra = table
            .clone()
            .try_into()
            .map_err(|e: toml::de::Error| SettingsError(e.message().to_string()))?;
        let chosen = name
            .map(str::to_string)
            .or(extra.name)
            .unwrap_or_else(|| default_name.to_string());
        check_runner_name(&chosen).map_err(SettingsError)?;
        if extra.mem_mb == Some(0) || extra.cpu == Some(0) {
            return Err(SettingsError("mem_mb and cpu must be above 0".into()));
        }
        let mem_mb = declared("mem_mb", extra.mem_mb, host.mem_mb)?;
        let cpu = declared("cpu", extra.cpu.map(u64::from), u64::from(host.cpu))?;
        table.insert("name".into(), toml::Value::String(chosen));
        let text = toml::to_string(&table).map_err(|e| SettingsError(e.to_string()))?;
        let mut settings = RunnerSettings::from_toml(&text, state_dir)?;
        settings.mem_mb = mem_mb;
        settings.cpu = u32::try_from(cpu).unwrap_or(u32::MAX);
        let secrets = extra
            .secret
            .iter()
            .map(|(n, e)| Ok((n.clone(), secret_from(n, &e.from)?)))
            .collect::<Result<_, SettingsError>>()?;
        Ok(RunnerConfig {
            settings,
            allow_elevated: extra.allow_elevated,
            programs: extra.programs,
            secrets,
        })
    }

    /// Runs the `[programs]` lookups and checks in the `PATH` jobs get (`[env] path`) and keeps
    /// only the programs that passed; returns one line per program that failed.
    pub fn check_programs(&mut self) -> Vec<String> {
        let search = self.settings.path.clone();
        let mut problems = Vec::new();
        self.settings.programs.clear();
        for (name, spec) in &self.programs {
            match check_program(name, spec, search.as_deref()) {
                Ok(()) => {
                    self.settings.programs.insert(name.clone());
                }
                Err(why) => problems.push(format!("program {name}: {why}")),
            }
        }
        problems
    }
}

/// The most runners one `--count` starts.
pub const MAX_COUNT: u32 = 64;

/// Names of `count` runners made from `base`: `base` alone for one, else `base-1` … `base-N`
/// (spec 11.1, `runner join --count`); `count` is 1 to [`MAX_COUNT`].
pub fn instance_names(base: &str, count: u32) -> Result<Vec<String>, String> {
    match count {
        0 => Err("--count must be at least 1".into()),
        1 => Ok(vec![base.to_string()]),
        n if n > MAX_COUNT => Err(format!("--count is at most {MAX_COUNT}")),
        n => Ok((1..=n).map(|i| format!("{base}-{i}")).collect()),
    }
}

fn windows_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let stem = stem.trim_end();
    matches!(stem, "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ["COM", "LPT"].iter().any(|p| {
            stem.strip_prefix(p).is_some_and(|n| {
                n.len() == 1 && n.bytes().all(|b| b.is_ascii_digit()) && n != "0"
                    || ["¹", "²", "³"].contains(&n)
            })
        })
}

/// A runner name is also the name of its state directory on every OS: besides the key name
/// rules it must not be made of dots only (the installation root or above it), must not be a
/// Windows device name (`CON`, `NUL`, `COM1`, `con.txt`, ...) and must not end with a dot or a
/// space, which Windows drops (`box.` would share the directory of `box`).
pub fn check_runner_name(name: &str) -> Result<(), String> {
    let invalid = || Err(format!("`{name}` is not a valid runner name"));
    if name.ends_with(['.', ' ']) || name.contains(['/', '\\']) || windows_reserved(name) {
        return invalid();
    }
    lr_net::check_name(name).map_err(|e| e.to_string())?;
    if name.chars().all(|c| c == '.') {
        return invalid();
    }
    Ok(())
}

/// The elevation guarantee (spec 3.12): a runner running as root, an elevated administrator
/// or `SYSTEM` starts only with `allow_elevated = true`.
pub fn elevation_allowed(elevated: bool, allow_elevated: bool) -> Result<(), String> {
    if elevated && !allow_elevated {
        return Err("the runner refuses to start as root or administrator; set allow_elevated = true in runner.toml to allow it (spec 3.12)"
            .into());
    }
    Ok(())
}

fn declared(key: &str, value: Option<u64>, host: u64) -> Result<u64, SettingsError> {
    match value {
        Some(v) if host > 0 && v > host => Err(SettingsError(format!(
            "{key} = {v} is more than this host has ({host}); declare at most {host}"
        ))),
        Some(v) => Ok(v),
        None if host == 0 => Err(SettingsError(format!(
            "the host's {key} could not be measured; declare {key} in runner.toml"
        ))),
        None => Ok(host),
    }
}

fn executable_names(name: &str) -> Vec<String> {
    if cfg!(windows) && Path::new(name).extension().is_none() {
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into());
        exts.split(';')
            .filter(|e| !e.is_empty())
            .map(|e| format!("{name}{}", e.to_ascii_lowercase()))
            .collect()
    } else {
        vec![name.to_string()]
    }
}

/// Where `name` would be found in `search` (a `PATH` value).
pub fn find_in_path(name: &str, search: &str) -> Option<PathBuf> {
    std::env::split_paths(search)
        .flat_map(|dir| executable_names(name).into_iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
}

fn check_program(name: &str, spec: &ProgramSpec, search: Option<&str>) -> Result<(), String> {
    match (&spec.path, search) {
        (Some(p), _) if !p.is_file() => return Err(format!("{} does not exist", p.display())),
        (Some(_), _) => {}
        (None, None) => {
            return Err("not looked up: runner.toml has no [env] path, the PATH of jobs".into())
        }
        (None, Some(search)) => {
            find_in_path(name, search).ok_or_else(|| "not found in PATH".to_string())?;
        }
    }
    let Some(check) = &spec.check else {
        return Ok(());
    };
    let timeout = Duration::from_secs(
        spec.check_timeout_s
            .unwrap_or(DEFAULT_CHECK_TIMEOUT_S)
            .max(1),
    );
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(check);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(check);
        c
    };
    cmd.env("PATH", search.unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("check cannot start: {e}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("check `{check}` ended with {status}")),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("check `{check}` took longer than {timeout:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    }
}
