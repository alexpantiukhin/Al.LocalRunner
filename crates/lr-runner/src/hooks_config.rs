use crate::settings::SettingsError;
use lr_abstractions::Enforce;
use lr_core::JobState;
use lr_proto::ReasonCode;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// Seconds a cleanup hook may run when `timeout_s` is not given.
pub const DEFAULT_CLEANUP_TIMEOUT_S: u64 = 60;

/// The longest timeout of a cleanup hook (one hour).
pub const MAX_CLEANUP_TIMEOUT_S: u64 = 3_600;

/// Default ceiling in percent of the declared value when the owner turns enforcement on in
/// `runner.toml` and neither the owner nor the broker gives a headroom.
pub const DEFAULT_HEADROOM_PCT: u32 = 125;

const RESERVED_ENV: [&str; 4] = ["PATH", "TEMP", "TMP", "TMPDIR"];
const RESERVED_PREFIX: &str = "LOCALRUNNER_";

/// How a job ended, for choosing the cleanup hooks to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CleanupOn {
    /// The job passed.
    Passed,
    /// The job failed on its own.
    Failed,
    /// The runner killed the job: an exceeded resource ceiling or a lost broker.
    Killed,
    /// The job was cancelled.
    Cancelled,
    /// The job timeout expired.
    Timeout,
}

impl CleanupOn {
    /// Every outcome.
    pub const ALL: [CleanupOn; 5] = [
        CleanupOn::Passed,
        CleanupOn::Failed,
        CleanupOn::Killed,
        CleanupOn::Cancelled,
        CleanupOn::Timeout,
    ];

    /// The outcome of a job that ended in `state` with `reason`.
    pub fn of(state: JobState, reason: Option<&ReasonCode>) -> CleanupOn {
        let killed = matches!(
            reason,
            Some(ReasonCode::ResourceExceeded | ReasonCode::BrokerUnavailable)
        );
        match state {
            JobState::Passed => CleanupOn::Passed,
            JobState::Timeout => CleanupOn::Timeout,
            _ if killed => CleanupOn::Killed,
            JobState::Cancelled => CleanupOn::Cancelled,
            _ => CleanupOn::Failed,
        }
    }
}

/// One `[[cleanup]]` entry of `runner.toml`: a command run after every job that ends as one of
/// `on`, in the job's environment plus `LOCALRUNNER_JOB_LABEL`. Its failure never changes the
/// job result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupHook {
    /// Name shown in the job log header `cleanup <name>`.
    pub name: String,
    /// Command line, not interpreted by the runner.
    pub command: String,
    /// Seconds the hook may run.
    pub timeout_s: u64,
    /// Job outcomes after which the hook runs.
    pub on: BTreeSet<CleanupOn>,
}

#[derive(Deserialize)]
pub(crate) struct RawCleanup {
    name: String,
    command: String,
    timeout_s: Option<u64>,
    on: Option<Vec<CleanupOn>>,
}

pub(crate) fn cleanup_hooks(raw: Vec<RawCleanup>) -> Result<Vec<CleanupHook>, SettingsError> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(raw.len());
    for r in raw {
        if r.name.trim().is_empty() || r.name.chars().any(char::is_control) {
            return Err(SettingsError(
                "[[cleanup]] name must be a non-empty line".into(),
            ));
        }
        if !seen.insert(r.name.clone()) {
            return Err(SettingsError(format!(
                "[[cleanup]] name {} is used twice",
                r.name
            )));
        }
        if r.command.trim().is_empty() {
            return Err(SettingsError(format!(
                "[[cleanup]] {} has an empty command",
                r.name
            )));
        }
        let timeout_s = r.timeout_s.unwrap_or(DEFAULT_CLEANUP_TIMEOUT_S);
        if timeout_s == 0 || timeout_s > MAX_CLEANUP_TIMEOUT_S {
            return Err(SettingsError(format!(
                "[[cleanup]] {} timeout_s must be 1 to {MAX_CLEANUP_TIMEOUT_S}",
                r.name
            )));
        }
        let on: BTreeSet<CleanupOn> = match r.on {
            None => CleanupOn::ALL.into_iter().collect(),
            Some(list) if list.is_empty() => {
                return Err(SettingsError(format!(
                    "[[cleanup]] {} has an empty `on` list",
                    r.name
                )))
            }
            Some(list) => list.into_iter().collect(),
        };
        out.push(CleanupHook {
            name: r.name,
            command: r.command,
            timeout_s,
            on,
        });
    }
    Ok(out)
}

pub(crate) fn job_env_table(
    raw: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, SettingsError> {
    for name in raw.keys() {
        let valid = !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(SettingsError(format!(
                "[job_env] {name} is not a valid variable name"
            )));
        }
        let upper = name.to_ascii_uppercase();
        if RESERVED_ENV.contains(&upper.as_str()) || upper.starts_with(RESERVED_PREFIX) {
            return Err(SettingsError(format!(
                "[job_env] {name} is reserved (PATH comes from [env] path; TEMP, TMP, TMPDIR and LOCALRUNNER_* belong to the runner)"
            )));
        }
    }
    Ok(raw)
}

/// Puts the owner's `[job_env]` into `env`, replacing a variable of the same name: the machine
/// owner has the last word over anything the job environment already holds.
pub fn apply_owner_env(env: &mut Vec<(String, String)>, owner: &BTreeMap<String, String>) {
    for (name, value) in owner {
        env.retain(|(n, _)| n != name);
        env.push((name.clone(), value.clone()));
    }
}

pub(crate) fn enforce_mode(text: &str) -> Result<Enforce, SettingsError> {
    match text {
        "off" => Ok(Enforce::Off),
        "warn" => Ok(Enforce::Warn),
        "kill" => Ok(Enforce::Kill),
        other => Err(SettingsError(format!(
            "[limits] enforce = \"{other}\" is unknown, use \"off\", \"warn\" or \"kill\""
        ))),
    }
}

pub(crate) fn headroom_pct(key: &str, factor: Option<f64>) -> Result<Option<u32>, SettingsError> {
    let Some(f) = factor else {
        return Ok(None);
    };
    if !f.is_finite() || !(1.0..=100.0).contains(&f) {
        return Err(SettingsError(format!(
            "[limits] {key} must be a factor from 1.0 to 100.0"
        )));
    }
    Ok(Some((f * 100.0).round() as u32))
}
