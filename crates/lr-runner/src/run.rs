use crate::agent::Shared;
use crate::cache::WORKSPACE_CLIENT;
use crate::job::{skip_steps, Final};
use crate::output::Forward;
use crate::prepare::Prepared;
use crate::source::{plan_source, SourceInput, SourcePlan, WorkspaceLayout};
use lr_abstractions::{
    CacheKey, JobOutcome, JobSpec, JobStep, Level, StepOutcome, StopSignal, DEFAULT_CANCEL_GRACE,
};
use lr_core::{JobState, StepState};
use lr_proto::{ReasonCode, WireCode};
use std::path::{Path, PathBuf};

struct Phase<'a> {
    steps: Vec<JobStep>,
    ords: Vec<Option<u32>>,
    timeout_s: u64,
    cwd: &'a Path,
    env: &'a [(String, String)],
}

async fn run_phase(
    shared: &Shared,
    p: &Prepared<'_>,
    phase: Phase<'_>,
    stop: StopSignal,
) -> JobOutcome {
    let spec = JobSpec {
        job: p.job.clone(),
        steps: phase.steps,
        timeout_s: phase.timeout_s,
        cwd: phase.cwd.to_path_buf(),
        env: phase.env.to_vec(),
        cancel_grace: DEFAULT_CANCEL_GRACE,
        header: vec![
            ("project".to_string(), p.spec.project.clone()),
            ("cwd".to_string(), phase.cwd.display().to_string()),
        ],
    };
    let forward = Forward::new(&shared.outbox, p.job.clone(), phase.ords);
    let outcome = shared.deps.process.run_job(spec, &forward, stop).await;
    forward.report_rest(&outcome).await;
    outcome
}

fn mirror_key(layout: &WorkspaceLayout, project: &str, url: &str) -> CacheKey {
    let project = layout
        .project_dir(project, url)
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    CacheKey {
        client: WORKSPACE_CLIENT.to_string(),
        project,
        name: "mirror".to_string(),
    }
}

fn ord(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

fn failed_step(outcome: &JobOutcome) -> Option<&StepOutcome> {
    outcome.steps.iter().find(|s| s.state == StepState::Failed)
}

struct Cleanup<'a> {
    shared: &'a Shared,
    checkout: Option<PathBuf>,
    mirror: Option<(CacheKey, PathBuf)>,
    caches: Vec<CacheKey>,
}

impl Cleanup<'_> {
    async fn run(self) {
        let s = self.shared;
        if let Some(dir) = &self.checkout {
            if let Err(e) = s.deps.files.remove_dir_all(dir).await {
                s.diag.event(
                    Level::Error,
                    "job checkout not removed",
                    &[
                        ("path", &dir.display().to_string()),
                        ("error", &e.to_string()),
                    ],
                );
            }
        }
        let caches = s.caches();
        let now = s.now();
        if let Some((key, path)) = self.mirror {
            caches.touch(key, &path, now).await;
        }
        for key in self.caches {
            let path = caches.layout.path(&key);
            caches.touch(key, &path, now).await;
        }
    }
}

async fn plan(shared: &Shared, p: &Prepared<'_>) -> Result<(SourcePlan, Option<PathBuf>), String> {
    let spec = p.spec;
    let layout = shared.workspace();
    let mirror = match &spec.code {
        WireCode::Commit {
            repo_url: Some(u), ..
        } => Some(layout.mirror_dir(&spec.project, u)),
        _ => None,
    };
    let mirror_exists = match &mirror {
        Some(m) => shared
            .deps
            .files
            .exists(m)
            .await
            .map_err(|e| format!("workspace unreadable: {}", e.0))?,
        None => false,
    };
    let input = SourceInput {
        job: &p.job,
        client: &spec.client,
        project: &spec.project,
        code: &spec.code,
        steps: &spec.steps,
        timeout_s: spec.requirements.timeout_s,
    };
    let plan = plan_source(&layout, &input, mirror_exists).map_err(|e| e.to_string())?;
    if let WireCode::WorkTree { cwd } = &spec.code {
        let there = shared
            .deps
            .files
            .exists(&plan.job_cwd)
            .await
            .map_err(|e| format!("work tree {cwd} unreadable: {}", e.0))?;
        if !there {
            return Err(format!("work tree {cwd} not found on this runner"));
        }
    }
    for dir in &plan.make_dirs {
        shared
            .deps
            .files
            .create_dir_all(dir)
            .await
            .map_err(|e| format!("workspace not created: {}", e.0))?;
    }
    Ok((plan, mirror))
}

pub(crate) async fn run_phases(shared: &Shared, p: Prepared<'_>, stop: StopSignal) -> Final {
    let spec = p.spec;
    let now = shared.now();
    let caches = shared.caches();
    let keys: Vec<CacheKey> = spec
        .requirements
        .cache
        .iter()
        .map(|name| CacheKey {
            client: spec.client.clone(),
            project: spec.project.clone(),
            name: name.clone(),
        })
        .collect();
    let mirror_url = match &spec.code {
        WireCode::Commit {
            repo_url: Some(u), ..
        } => Some(u.as_str()),
        _ => None,
    };
    let layout = shared.workspace();
    let mirror_key = mirror_url.map(|u| mirror_key(&layout, &spec.project, u));
    shared
        .claim(keys.iter().cloned().chain(mirror_key.clone()).collect())
        .await;
    shared.sweep().await;
    if let Err(detail) = shared.reserve_disk(spec).await {
        skip_steps(shared, &p.job, &spec.steps, 0).await;
        return Final {
            reason: Some(ReasonCode::DiskInsufficient),
            ..Final::failed(detail)
        };
    }
    let mut env = p.env.clone();
    match caches
        .open(&spec.client, &spec.project, &spec.requirements.cache, now)
        .await
    {
        Ok((_, cache_env)) => env.extend(cache_env),
        Err(detail) => {
            skip_steps(shared, &p.job, &spec.steps, 0).await;
            return Final::failed(detail);
        }
    }
    let (plan, mirror) = match plan(shared, &p).await {
        Ok(x) => x,
        Err(detail) => {
            skip_steps(shared, &p.job, &spec.steps, 0).await;
            return Final::failed(detail);
        }
    };
    let cleanup = Cleanup {
        shared,
        checkout: plan.remove_after.clone(),
        mirror: mirror_key.zip(mirror),
        caches: keys,
    };
    let started = shared.now();
    let total_s = spec.requirements.timeout_s;
    if !plan.source_steps.is_empty() {
        let n = plan.source_steps.len();
        let (steps, ords) = if plan.from_profile {
            (
                p.steps(0, &plan.source_steps),
                (0..n).map(|i| Some(ord(i))).collect(),
            )
        } else {
            (
                plan.source_steps
                    .iter()
                    .cloned()
                    .map(JobStep::from)
                    .collect(),
                vec![None; n],
            )
        };
        let phase = Phase {
            steps,
            ords,
            timeout_s: total_s,
            cwd: &plan.source_cwd,
            env: &env,
        };
        let out = run_phase(shared, &p, phase, stop.clone()).await;
        if out.state != JobState::Passed {
            skip_steps(shared, &p.job, &spec.steps, plan.first_ord).await;
            cleanup.run().await;
            let detail = match (out.state, plan.from_profile, failed_step(&out)) {
                (JobState::Failed, false, Some(s)) => {
                    Some(format!("source step failed: {}", s.name))
                }
                _ => None,
            };
            return Final {
                state: out.state,
                exit_code: out.exit_code,
                reason: None,
                detail,
            };
        }
    }
    let spent_ms = shared.now().millis_since(started);
    let left_s = total_s
        .saturating_mul(1000)
        .saturating_sub(spent_ms)
        .div_ceil(1000);
    let rest = &spec.steps[plan.first_ord.min(spec.steps.len())..];
    let phase = Phase {
        steps: p.steps(plan.first_ord, rest),
        ords: (plan.first_ord..plan.first_ord + rest.len())
            .map(|o| Some(ord(o)))
            .collect(),
        timeout_s: left_s,
        cwd: &plan.job_cwd,
        env: &env,
    };
    let out = run_phase(shared, &p, phase, stop).await;
    cleanup.run().await;
    Final {
        state: out.state,
        exit_code: out.exit_code,
        reason: None,
        detail: None,
    }
}
