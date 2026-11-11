//! `[service.*]` sections of `runner.toml` (spec 3.11): what the owner declares about each
//! runner service. The core does not know what program runs inside.

use crate::settings::SettingsError;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Seconds the readiness check may take when `ready_timeout_s` is not set.
pub const DEFAULT_READY_TIMEOUT_S: u64 = 60;

/// Seconds a service gets to end after `stop` before its tree is killed, when
/// `stop_timeout_s` is not set.
pub const DEFAULT_STOP_TIMEOUT_S: u64 = 10;

/// Restarts after failures when `restart` is not set; one more failure makes the service
/// unavailable.
pub const DEFAULT_RESTARTS: u32 = 3;

/// When a warm service is stopped (`lifetime`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifetime {
    /// `per_job`: right after the job that started it.
    PerJob,
    /// `idle`: after `idle_timeout_s` without a job.
    Idle,
    /// `keep`: lives while the runner lives.
    Keep,
}

/// Who may use a warm service started for another job (`reuse`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reuse {
    /// `same_client`: jobs of the same client key only (the default).
    SameClient,
    /// `same_project`: jobs of the same project, whatever the client.
    SameProject,
    /// `any`: any job.
    Any,
}

/// One declared service. Every service ends by `max_lifetime_s` at the latest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    /// `start`: command line of the long-lived process.
    pub start: String,
    /// `ready_check`: command that exits 0 once the service is ready; repeated until then.
    pub ready_check: String,
    /// `stop`: command asking the service to end; without it the tree is killed.
    pub stop: Option<String>,
    /// `ready_timeout_s`: how long readiness may take.
    pub ready_timeout_s: u64,
    /// `stop_timeout_s`: how long the service gets to end after `stop`.
    pub stop_timeout_s: u64,
    /// `memory_mb`: memory the live service holds in the pool budget.
    pub memory_mb: u64,
    /// `lifetime`.
    pub lifetime: Lifetime,
    /// `idle_timeout_s`: required for [`Lifetime::Idle`].
    pub idle_timeout_s: Option<u64>,
    /// `max_lifetime_s`: mandatory for every service.
    pub max_lifetime_s: u64,
    /// `restart`: restarts allowed after failures before the service becomes unavailable.
    pub restart: u32,
    /// `reuse`, already narrowed to [`Reuse::SameClient`] for a service with secrets
    /// (spec 3.13).
    pub reuse: Reuse,
    /// `secrets`: names of runner secrets the service explicitly requests.
    pub secrets: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawService {
    start: Option<String>,
    ready_check: Option<String>,
    stop: Option<String>,
    ready_timeout_s: Option<u64>,
    stop_timeout_s: Option<u64>,
    memory_mb: Option<u64>,
    lifetime: Option<String>,
    idle_timeout_s: Option<u64>,
    max_lifetime_s: Option<u64>,
    restart: Option<u32>,
    reuse: Option<String>,
    #[serde(default)]
    secrets: Vec<String>,
}

fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.chars().all(|c| c == '.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn command(name: &str, key: &str, value: Option<String>) -> Result<String, SettingsError> {
    value
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| SettingsError(format!("[service.{name}] {key} is required")))
}

fn positive(name: &str, key: &str, value: u64) -> Result<u64, SettingsError> {
    if value == 0 {
        return Err(SettingsError(format!(
            "[service.{name}] {key} must be above 0"
        )));
    }
    Ok(value)
}

fn lifetime(name: &str, value: Option<&str>) -> Result<Lifetime, SettingsError> {
    match value.unwrap_or("per_job") {
        "per_job" => Ok(Lifetime::PerJob),
        "idle" => Ok(Lifetime::Idle),
        "keep" => Ok(Lifetime::Keep),
        other => Err(SettingsError(format!(
            "[service.{name}] lifetime = \"{other}\" is unknown: per_job, idle or keep"
        ))),
    }
}

fn reuse(name: &str, value: Option<&str>) -> Result<Reuse, SettingsError> {
    match value.unwrap_or("same_client") {
        "same_client" => Ok(Reuse::SameClient),
        "same_project" => Ok(Reuse::SameProject),
        "any" => Ok(Reuse::Any),
        other => Err(SettingsError(format!(
            "[service.{name}] reuse = \"{other}\" is unknown: same_client, same_project or any"
        ))),
    }
}

fn service(name: &str, raw: RawService) -> Result<ServiceSpec, SettingsError> {
    if !plain_name(name) {
        return Err(SettingsError(format!(
            "[service.{name}]: a service name is letters, digits, '-', '_' and '.'"
        )));
    }
    let max_lifetime_s = raw.max_lifetime_s.ok_or_else(|| {
        SettingsError(format!(
            "[service.{name}] max_lifetime_s is required: no service lives without a bound"
        ))
    })?;
    let lifetime = lifetime(name, raw.lifetime.as_deref())?;
    let idle_timeout_s = match raw.idle_timeout_s {
        Some(s) => Some(positive(name, "idle_timeout_s", s)?),
        None if lifetime == Lifetime::Idle => {
            return Err(SettingsError(format!(
                "[service.{name}] lifetime = \"idle\" needs idle_timeout_s"
            )))
        }
        None => None,
    };
    let secrets = raw.secrets;
    let mut reuse = reuse(name, raw.reuse.as_deref())?;
    if !secrets.is_empty() {
        reuse = Reuse::SameClient;
    }
    Ok(ServiceSpec {
        start: command(name, "start", raw.start)?,
        ready_check: command(name, "ready_check", raw.ready_check)?,
        stop: raw.stop.filter(|s| !s.trim().is_empty()),
        ready_timeout_s: positive(
            name,
            "ready_timeout_s",
            raw.ready_timeout_s.unwrap_or(DEFAULT_READY_TIMEOUT_S),
        )?,
        stop_timeout_s: positive(
            name,
            "stop_timeout_s",
            raw.stop_timeout_s.unwrap_or(DEFAULT_STOP_TIMEOUT_S),
        )?,
        memory_mb: raw.memory_mb.unwrap_or(0),
        lifetime,
        idle_timeout_s,
        max_lifetime_s: positive(name, "max_lifetime_s", max_lifetime_s)?,
        restart: raw.restart.unwrap_or(DEFAULT_RESTARTS),
        reuse,
        secrets,
    })
}

pub(crate) fn services(
    raw: BTreeMap<String, RawService>,
) -> Result<BTreeMap<String, ServiceSpec>, SettingsError> {
    raw.into_iter()
        .map(|(name, r)| service(&name, r).map(|s| (name, s)))
        .collect()
}
