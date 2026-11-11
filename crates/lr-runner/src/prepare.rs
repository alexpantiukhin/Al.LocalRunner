use crate::agent::Shared;
use crate::job::Final;
use lr_abstractions::{JobStep, Level, SecretValue};
use lr_core::{JobId, StepSpec};
use lr_proto::{AssignSpec, ReasonCode};
use std::collections::BTreeMap;

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
    env
}
