use lr_abstractions::Files;
use lr_core::{JobId, Timestamp};
use serde::Serialize;
use std::path::{Component, Path, PathBuf};

pub(crate) fn checked_dir(dir: &Path) -> Result<(), String> {
    let absolute = dir.is_absolute();
    let clean = dir
        .components()
        .all(|c| !matches!(c, Component::ParentDir | Component::CurDir));
    if !absolute || !clean {
        return Err(format!(
            "directory {} must be absolute and without . or ..",
            dir.display()
        ));
    }
    Ok(())
}

fn safe_id(job: &JobId) -> Result<&str, String> {
    let id = job.0.as_str();
    let ok = !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if ok {
        Ok(id)
    } else {
        Err(format!("job id `{id}` is not a safe file name"))
    }
}

fn job_file(cwd: &Path, job: &JobId, ext: &str) -> Result<PathBuf, String> {
    checked_dir(cwd)?;
    let id = safe_id(job)?;
    Ok(cwd.join("temp").join("runner").join(format!("{id}.{ext}")))
}

pub(crate) fn handle_path(cwd: &Path, job: &JobId) -> Result<PathBuf, String> {
    job_file(cwd, job, "json")
}

pub(crate) fn log_path(cwd: &Path, job: &JobId) -> Result<PathBuf, String> {
    job_file(cwd, job, "log")
}

#[derive(Serialize)]
struct Handle<'a> {
    job: &'a str,
    label: &'a str,
    profile: &'a str,
    submitted_at: u64,
    detached: bool,
}

pub(crate) async fn write_handle(
    files: &dyn Files,
    path: &Path,
    job: &JobId,
    label: &str,
    profile: &str,
    at: Timestamp,
    detached: bool,
) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(&Handle {
        job: &job.0,
        label,
        profile,
        submitted_at: at.0,
        detached,
    })
    .map_err(|e| e.to_string())?;
    files.write(path, &body).await.map_err(|e| e.to_string())
}
