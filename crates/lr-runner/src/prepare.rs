use crate::agent::Shared;
use crate::job::Final;
use lr_abstractions::{JobStep, Level, SecretValue};
use lr_core::{JobId, StepSpec};
use lr_proto::{AssignSpec, ReasonCode};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(crate) struct Prepared<'a> {
    pub(crate) job: JobId,
    pub(crate) spec: &'a AssignSpec,
    pub(crate) secrets: Vec<Vec<(String, SecretValue)>>,
    pub(crate) env: Vec<(String, String)>,
}

impl Prepared<'_> {
    pub(crate) fn steps(&self, from: usize, specs: &[StepSpec]) -> Vec<JobStep> {
        specs
            .iter()
            .enumerate()
            .map(|(i, spec)| JobStep {
                spec: spec.clone(),
                secrets: self.secrets.get(from + i).cloned().unwrap_or_default(),
            })
            .collect()
    }
}

pub(crate) fn resolve_commands(
    named: &BTreeMap<String, String>,
    spec: &mut AssignSpec,
) -> Result<(), String> {
    if !spec.named_commands {
        return Ok(());
    }
    for step in &mut spec.steps {
        let Some(line) = named.get(&step.command) else {
            return Err(format!(
                "command {} is not named in runner.toml [commands]; only named commands run for this client",
                step.command
            ));
        };
        step.command = line.clone();
    }
    Ok(())
}

pub(crate) fn secret_var(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

const RESERVED: [&str; 4] = ["PATH", "HOME", "TEMP", "TMP"];
const RESERVED_PREFIX: &str = "LOCALRUNNER_";

fn reserved(var: &str) -> bool {
    RESERVED.contains(&var) || var.starts_with(RESERVED_PREFIX)
}

pub(crate) async fn step_secrets(
    shared: &Shared,
    spec: &AssignSpec,
) -> Result<Vec<Vec<(String, SecretValue)>>, Final> {
    let admitted = &spec.secrets;
    let by_step: Vec<&str> = spec
        .steps
        .iter()
        .flat_map(|s| s.secrets.iter().map(String::as_str))
        .collect();
    for name in admitted
        .iter()
        .map(String::as_str)
        .chain(by_step.iter().copied())
    {
        let var = secret_var(name);
        if reserved(&var) {
            return Err(Final::failed(format!(
                "secret {name} would replace the reserved variable {var}"
            )));
        }
    }
    let mut values: BTreeMap<&str, SecretValue> = BTreeMap::new();
    for name in admitted {
        match shared.deps.secrets.get(name).await {
            Ok(v) => {
                values.insert(name, v);
            }
            Err(e) => {
                shared.diag.event(
                    Level::Warn,
                    "secret unavailable, job not run",
                    &[("secret", name)],
                );
                return Err(Final {
                    reason: Some(ReasonCode::SecretMissing),
                    ..Final::failed(e.to_string())
                });
            }
        }
    }
    let job_wide: Vec<&str> = admitted
        .iter()
        .map(String::as_str)
        .filter(|n| !by_step.contains(n))
        .collect();
    let mut out = Vec::with_capacity(spec.steps.len());
    for step in &spec.steps {
        let mut list = Vec::new();
        for name in step
            .secrets
            .iter()
            .map(String::as_str)
            .chain(job_wide.iter().copied())
        {
            let Some(v) = values.get(name) else {
                return Err(Final {
                    reason: Some(ReasonCode::SecretMissing),
                    ..Final::failed(format!("secret not admitted: {name}"))
                });
            };
            if !list
                .iter()
                .any(|(n, _): &(String, SecretValue)| *n == secret_var(name))
            {
                list.push((secret_var(name), v.clone()));
            }
        }
        out.push(list);
    }
    Ok(out)
}

pub(crate) fn base_env(shared: &Shared) -> Vec<(String, String)> {
    let s = &shared.deps.settings;
    let mut env = Vec::new();
    for (name, value) in [("PATH", &s.path), ("HOME", &s.home), ("TEMP", &s.temp)] {
        if let Some(v) = value {
            env.push((name.to_string(), v.clone()));
        }
    }
    env
}

pub(crate) fn job_env(shared: &Shared, job: &JobId) -> Vec<(String, String)> {
    let mut env = base_env(shared);
    env.push(("LOCALRUNNER_JOB_ID".to_string(), job.0.clone()));
    if !shared.deps.settings.cleanup.is_empty() {
        env.push(("LOCALRUNNER_JOB_LABEL".to_string(), job_label(job)));
    }
    env
}

/// Directory of the runner's state that holds the private temporary directories of jobs; what
/// a crash leaves in it is removed when the runner starts.
pub(crate) fn jobs_temp(shared: &Shared) -> PathBuf {
    shared.deps.settings.state_dir.join("tmp")
}

/// The private temporary directory of a job: what the job writes there is measured against its
/// `disk_mb` and removed with it.
pub(crate) fn job_temp(shared: &Shared, job: &JobId) -> PathBuf {
    jobs_temp(shared).join(job_label(job))
}

/// Points `TEMP`, `TMP` and `TMPDIR` of a job at its private temporary directory.
pub(crate) fn apply_job_temp(env: &mut Vec<(String, String)>, temp: &Path) {
    let value = temp.to_string_lossy().into_owned();
    env.retain(|(k, _)| !["TEMP", "TMP", "TMPDIR"].contains(&k.as_str()));
    for name in ["TEMP", "TMP", "TMPDIR"] {
        env.push((name.to_string(), value.clone()));
    }
}

/// A short label unique per job, for cleanup hooks to find what the job left behind.
pub(crate) fn job_label(job: &JobId) -> String {
    format!("lr-{}", job.0)
}
