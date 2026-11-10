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
    /// `host`: one runner runs one job at a time and gives it everything it declares.
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
        lr_net::check_name(&chosen).map_err(|e| SettingsError(e.to_string()))?;
        if extra.mem_mb == Some(0) || extra.cpu == Some(0) {
            return Err(SettingsError("mem_mb and cpu must be above 0".into()));
        }
        table.insert("name".into(), toml::Value::String(chosen));
        let text = toml::to_string(&table).map_err(|e| SettingsError(e.to_string()))?;
        let mut settings = RunnerSettings::from_toml(&text, state_dir)?;
        settings.mem_mb = extra.mem_mb.unwrap_or(host.mem_mb);
        settings.cpu = extra.cpu.unwrap_or(host.cpu);
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

    /// Runs the `[programs]` lookups and checks and keeps only the programs that passed;
    /// returns one line per program that failed.
    pub fn check_programs(&mut self) -> Vec<String> {
        let search = self
            .settings
            .path
            .clone()
            .or_else(|| std::env::var("PATH").ok())
            .unwrap_or_default();
        let mut problems = Vec::new();
        self.settings.programs.clear();
        for (name, spec) in &self.programs {
            match check_program(name, spec, &search) {
                Ok(()) => {
                    self.settings.programs.insert(name.clone());
                }
                Err(why) => problems.push(format!("program {name}: {why}")),
            }
        }
        problems
    }
}

/// Names of `count` runners made from `base`: `base` alone for one, else `base-1` … `base-N`
/// (spec 11.1, `runner join --count`).
pub fn instance_names(base: &str, count: u32) -> Vec<String> {
    if count <= 1 {
        return vec![base.to_string()];
    }
    (1..=count).map(|i| format!("{base}-{i}")).collect()
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

fn check_program(name: &str, spec: &ProgramSpec, search: &str) -> Result<(), String> {
    match &spec.path {
        Some(p) if !p.is_file() => return Err(format!("{} does not exist", p.display())),
        Some(_) => {}
        None => {
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
    cmd.env("PATH", search)
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
