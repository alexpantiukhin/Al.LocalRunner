use lr_proto::WireCapabilities;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

/// Seconds after which a runner kills its jobs when the broker is unreachable, when neither the
/// broker nor `runner.toml` says otherwise (spec 3.3: 3 minutes).
pub const DEFAULT_LOST_KILL_S: u64 = 180;

/// The longest grace a broker may set for killing jobs while it is unreachable (one day);
/// larger values are clamped to it.
pub const MAX_LOST_KILL_S: u64 = 86_400;

/// The longest heartbeat interval a broker may set (one hour); its `heartbeat_s` is clamped to
/// `1..=MAX_HEARTBEAT_S`.
pub const MAX_HEARTBEAT_S: u64 = 3_600;

const MB_PER_GB: u64 = 1024;

/// Size and age limits of the cache or of the workspace (spec 3.10); `None` is no limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Retention {
    /// `max_size_gb`: above it the least recently used entries go first (`evict = "lru"`).
    pub max_size_gb: Option<u64>,
    /// `retention_days`: entries unused for longer are removed.
    pub retention_days: Option<u64>,
}

impl Retention {
    /// The size limit in bytes.
    pub fn max_bytes(&self) -> Option<u64> {
        self.max_size_gb
            .map(|gb| gb.saturating_mul(MB_PER_GB * 1024 * 1024))
    }
}

/// The owner's settings of one runner (`runner.toml`, spec 3.10) plus the machine facts the
/// assembly detects (memory, CPU, disk, programs found, environment whitelist values).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerSettings {
    /// Runner name, unique on the broker.
    pub name: String,
    /// State directory: workspace mirrors, caches and small state files live inside.
    pub state_dir: PathBuf,
    /// Opaque platform labels.
    pub platforms: BTreeSet<String>,
    /// Declared tags.
    pub tags: BTreeSet<String>,
    /// Programs found and checked.
    pub programs: BTreeSet<String>,
    /// Memory capacity in MB.
    pub mem_mb: u64,
    /// CPU capacity.
    pub cpu: u32,
    /// Free disk in MB.
    pub disk_mb: u64,
    /// `[accept] min_mem_mb`.
    pub accept_min_mem_mb: Option<u64>,
    /// `[limits] max_job_timeout_s`: the timeout ceiling; `None` for none.
    pub max_job_timeout_s: Option<u64>,
    /// `[cache]`.
    pub cache: Retention,
    /// `[workspace]`.
    pub workspace: Retention,
    /// `[grace] broker_lost_kill_s`: may only shorten the broker's value.
    pub broker_lost_kill_s: Option<u64>,
    /// `[env] path`: `PATH` of every job.
    pub path: Option<String>,
    /// `HOME` of every job.
    pub home: Option<String>,
    /// `TEMP` of every job.
    pub temp: Option<String>,
}

/// Why `runner.toml` was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsError(pub String);

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "runner.toml: {}", self.0)
    }
}

impl std::error::Error for SettingsError {}

#[derive(Deserialize, Default)]
struct RetentionFile {
    max_size_gb: Option<u64>,
    retention_days: Option<u64>,
    evict: Option<String>,
}

#[derive(Deserialize, Default)]
struct Accept {
    min_mem_mb: Option<u64>,
}

#[derive(Deserialize, Default)]
struct Limits {
    max_job_timeout_s: Option<u64>,
}

#[derive(Deserialize, Default)]
struct Grace {
    broker_lost_kill_s: Option<u64>,
}

#[derive(Deserialize, Default)]
struct Env {
    path: Option<String>,
}

#[derive(Deserialize)]
struct File {
    name: String,
    #[serde(default)]
    platforms: BTreeSet<String>,
    #[serde(default)]
    tags: BTreeSet<String>,
    #[serde(default)]
    accept: Accept,
    #[serde(default)]
    programs: BTreeMap<String, toml::Value>,
    #[serde(default)]
    limits: Limits,
    #[serde(default)]
    cache: RetentionFile,
    #[serde(default)]
    workspace: RetentionFile,
    #[serde(default)]
    grace: Grace,
    #[serde(default)]
    env: Env,
}

fn retention(section: &str, f: RetentionFile) -> Result<Retention, SettingsError> {
    if let Some(e) = f.evict.as_deref().filter(|e| *e != "lru") {
        return Err(SettingsError(format!(
            "[{section}] evict = \"{e}\" is unknown, only \"lru\" exists"
        )));
    }
    Ok(Retention {
        max_size_gb: f.max_size_gb,
        retention_days: f.retention_days,
    })
}

impl RunnerSettings {
    /// A runner with no labels, no limits and zero capacity.
    pub fn new(name: &str, state_dir: PathBuf) -> RunnerSettings {
        RunnerSettings {
            name: name.to_string(),
            state_dir,
            platforms: BTreeSet::new(),
            tags: BTreeSet::new(),
            programs: BTreeSet::new(),
            mem_mb: 0,
            cpu: 0,
            disk_mb: 0,
            accept_min_mem_mb: None,
            max_job_timeout_s: None,
            cache: Retention::default(),
            workspace: Retention::default(),
            broker_lost_kill_s: None,
            path: None,
            home: None,
            temp: None,
        }
    }

    /// Reads `runner.toml`. Sections the runner agent does not use (`[service.*]`,
    /// `[secret.*]`) are left to their owners; `[programs]` gives the names only, their
    /// checks run in the assembly.
    pub fn from_toml(text: &str, state_dir: PathBuf) -> Result<RunnerSettings, SettingsError> {
        let f: File = toml::from_str(text).map_err(|e| SettingsError(e.message().to_string()))?;
        if f.limits.max_job_timeout_s == Some(0) {
            return Err(SettingsError(
                "[limits] max_job_timeout_s must be above 0".into(),
            ));
        }
        let mut s = RunnerSettings::new(&f.name, state_dir);
        s.platforms = f.platforms;
        s.tags = f.tags;
        s.programs = f.programs.into_keys().collect();
        s.accept_min_mem_mb = f.accept.min_mem_mb;
        s.max_job_timeout_s = f.limits.max_job_timeout_s;
        s.cache = retention("cache", f.cache)?;
        s.workspace = retention("workspace", f.workspace)?;
        s.broker_lost_kill_s = f.grace.broker_lost_kill_s;
        s.path = f.env.path;
        Ok(s)
    }

    /// Seconds after which jobs are killed while the broker is unreachable: the broker's value
    /// (3 minutes without one, at most [`MAX_LOST_KILL_S`]), shortened by `[grace]
    /// broker_lost_kill_s` but never lengthened.
    pub fn lost_kill_s(&self, broker: Option<u64>) -> u64 {
        let base = broker.unwrap_or(DEFAULT_LOST_KILL_S).min(MAX_LOST_KILL_S);
        self.broker_lost_kill_s.map_or(base, |own| own.min(base))
    }

    /// What the runner offers, with the timeout ceiling, the cache limit and `cache_used_mb`.
    pub fn capabilities(&self, cache_used_mb: Option<u64>) -> WireCapabilities {
        WireCapabilities {
            platforms: self.platforms.clone(),
            mem_mb: self.mem_mb,
            cpu: self.cpu,
            disk_mb: self.disk_mb,
            programs: self.programs.clone(),
            tags: self.tags.clone(),
            max_job_timeout_s: self.max_job_timeout_s,
            services: BTreeMap::new(),
            machine: None,
            accept_min_mem_mb: self.accept_min_mem_mb,
            cache_max_mb: self
                .cache
                .max_size_gb
                .map(|gb| gb.saturating_mul(MB_PER_GB)),
            cache_used_mb,
        }
    }
}
