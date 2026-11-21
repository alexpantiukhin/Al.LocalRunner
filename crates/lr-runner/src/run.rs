use crate::agent::Shared;
use crate::hooks_config::apply_owner_env;
use crate::job::{skip_steps, Final};
use crate::limits::{exceeded_detail, job_limits};
use crate::output::Forward;
use crate::prepare::{job_temp, Prepared};
use crate::source::{plan_source, SourcePlan};
use lr_abstractions::{
    CacheKey, JobOutcome, JobSpec, JobStep, Level, StopSignal, Usage, DEFAULT_CANCEL_GRACE,
};
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
    let mut dirs = Vec::new();
    if !matches!(p.spec.code, WireCode::WorkTree { .. }) {
        dirs.push(phase.cwd.to_path_buf());
    }
    dirs.push(job_temp(shared, &p.job));
    let limits = job_limits(
        &p.spec.requirements,
        p.spec.limits.as_ref(),
        &shared.job_settings(),
        dirs,
    );
    let spec = JobSpec {
        job: p.job.clone(),
        steps: phase.steps,
        timeout_s: phase.timeout_s,
        cwd: phase.cwd.to_path_buf(),
        env: phase.env.to_vec(),
        cancel_grace: DEFAULT_CANCEL_GRACE,
        limits,
        header: vec![
            ("project".to_string(), p.spec.project.clone()),
            ("cwd".to_string(), phase.cwd.display().to_string()),
        ],
    };
    let forward = Forward::new(shared, p.job.clone(), phase.ords);
    let outcome = shared.deps.process.run_job(spec, &forward, stop).await;
    forward.report_rest(&outcome).await;
    outcome
}

fn merge_peaks(into: &mut Usage, from: &Usage) {
    into.peak_mem_mb = into.peak_mem_mb.max(from.peak_mem_mb);
    into.peak_disk_mb = into.peak_disk_mb.max(from.peak_disk_mb);
}

fn final_of(out: &JobOutcome, peaks: &Usage, detail: Option<String>) -> Final {
    let (reason, detail) = match &out.exceeded {
        Some(e) => (Some(ReasonCode::ResourceExceeded), Some(exceeded_detail(e))),
        None => (None, detail),
    };
    Final {
        state: out.state,
        exit_code: out.exit_code,
        reason,
        detail,
        peak_mem_mb: peaks.peak_mem_mb,
        peak_disk_mb: peaks.peak_disk_mb,
    }
}

fn ord(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

const TEMP_RETRIES: u32 = 8;
const TEMP_RETRY_PAUSE: std::time::Duration = std::time::Duration::from_millis(250);

struct Cleanup<'a> {
    shared: &'a Shared,
    temp: PathBuf,
    own: Option<PathBuf>,
    caches: Vec<CacheKey>,
}

impl Cleanup<'_> {
    async fn run(self) {
        let s = self.shared;
        if let Some(own) = &self.own {
            if let Err(e) = s.deps.files.remove_dir_all(own).await {
                s.diag.event(
                    Level::Error,
                    "job folder not removed",
                    &[
                        ("path", &own.display().to_string()),
                        ("error", &e.to_string()),
                    ],
                );
            }
        }
        let dir = &self.temp;
        let mut removed = s.deps.files.remove_dir_all(dir).await;
        for _ in 0..TEMP_RETRIES {
            if removed.is_ok() {
                break;
            }
            s.deps.clock.sleep(TEMP_RETRY_PAUSE).await;
            removed = s.deps.files.remove_dir_all(dir).await;
        }
        if let Err(e) = removed {
            s.diag.event(
                Level::Error,
                "job temp not removed",
                &[
                    ("path", &dir.display().to_string()),
                    ("error", &e.to_string()),
                ],
            );
        }
        let caches = s.caches();
        let now = s.now();
        for key in self.caches {
            let path = caches.layout.path(&key);
            caches.touch(key, &path, now).await;
        }
    }
}

async fn plan(shared: &Shared, p: &Prepared<'_>) -> Result<SourcePlan, String> {
    let spec = p.spec;
    let plan = plan_source(
        &shared.workspace(),
        &spec.code,
        &spec.project,
        &spec.client,
        &p.job.0,
    );
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
    Ok(plan)
}

pub(crate) async fn run_phases(shared: &Shared, p: Prepared<'_>, stop: StopSignal) -> Final {
    let spec = p.spec;
    let now = shared.now();
    let caches = shared.caches();
    let cache_client = spec.cache_client.as_deref().unwrap_or(&spec.client);
    let keys: Vec<CacheKey> = spec
        .requirements
        .cache
        .iter()
        .map(|name| CacheKey {
            client: cache_client.to_string(),
            project: spec.project.clone(),
            name: name.clone(),
        })
        .collect();
    shared.claim(keys.clone()).await;
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
        .open(cache_client, &spec.project, &spec.requirements.cache, now)
        .await
    {
        Ok((_, cache_env)) => {
            env.extend(cache_env);
            apply_owner_env(&mut env, &shared.job_settings().job_env);
        }
        Err(detail) => {
            skip_steps(shared, &p.job, &spec.steps, 0).await;
            return Final::failed(detail);
        }
    }
    let plan = match plan(shared, &p).await {
        Ok(x) => x,
        Err(detail) => {
            skip_steps(shared, &p.job, &spec.steps, 0).await;
            return Final::failed(detail);
        }
    };
    let temp = job_temp(shared, &p.job);
    if let Err(e) = shared.deps.files.create_dir_all(&temp).await {
        skip_steps(shared, &p.job, &spec.steps, 0).await;
        return Final::failed(format!("job temp directory not created: {}", e.0));
    }
    let cleanup = Cleanup {
        shared,
        temp,
        own: plan.make_dirs.first().cloned(),
        caches: keys,
    };
    let mut peaks = Usage::default();
    let total_s = spec.requirements.timeout_s;
    let phase = Phase {
        steps: p.steps(0, &spec.steps),
        ords: (0..spec.steps.len()).map(|o| Some(ord(o))).collect(),
        timeout_s: total_s,
        cwd: &plan.job_cwd,
        env: &env,
    };
    let out = run_phase(shared, &p, phase, stop).await;
    merge_peaks(&mut peaks, &out.usage);
    cleanup.run().await;
    final_of(&out, &peaks, None)
}
