use crate::identity::{
    build_identity, sanitize_label, validate_label, GithubInfo, Identity, IdentityError, Source,
};
use std::collections::BTreeMap;
use std::path::Path;

const DEFAULT_SERVER_URL: &str = "https://github.com";

fn required<'a>(env: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str, IdentityError> {
    env.get(name)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| IdentityError::MissingEnv(name.to_string()))
}

fn optional(env: &BTreeMap<String, String>, name: &str) -> String {
    env.get(name)
        .map(|v| v.trim().to_string())
        .unwrap_or_default()
}

/// Collects the identity of a GitHub Actions job from its environment. An explicit `label`
/// (`--as`) is validated; without it the label is `github:<workflow>/<job>`. Requires
/// `GITHUB_ACTIONS=true`.
pub fn collect_github(
    env: &BTreeMap<String, String>,
    label: Option<&str>,
    cwd: &Path,
) -> Result<Identity, IdentityError> {
    if env.get("GITHUB_ACTIONS").map(|v| v.trim()) != Some("true") {
        return Err(IdentityError::NotGithubActions);
    }
    let repository = required(env, "GITHUB_REPOSITORY")?.to_string();
    let workflow = required(env, "GITHUB_WORKFLOW")?.to_string();
    let job = required(env, "GITHUB_JOB")?.to_string();
    let run_id: u64 = required(env, "GITHUB_RUN_ID")?
        .parse()
        .map_err(|_| IdentityError::MissingEnv("GITHUB_RUN_ID".to_string()))?;
    let attempt = match optional(env, "GITHUB_RUN_ATTEMPT").as_str() {
        "" => None,
        text => Some(
            text.parse::<u64>()
                .map_err(|_| IdentityError::MissingEnv("GITHUB_RUN_ATTEMPT".to_string()))?,
        ),
    };
    let server = optional(env, "GITHUB_SERVER_URL");
    let server = match server.trim_end_matches('/') {
        "" => DEFAULT_SERVER_URL,
        s => s,
    };
    let mut run_url = format!("{server}/{repository}/actions/runs/{run_id}");
    if let Some(attempt) = attempt {
        run_url.push_str(&format!("/attempts/{attempt}"));
    }
    let label = match label {
        Some(explicit) => validate_label(explicit)?,
        None => sanitize_label(&format!("github:{workflow}/{job}")),
    };
    let info = GithubInfo {
        repository: repository.clone(),
        workflow,
        job,
        run_id,
        attempt,
        git_ref: optional(env, "GITHUB_REF"),
        actor: optional(env, "GITHUB_ACTOR"),
        run_url,
    };
    Ok(build_identity(
        label,
        None,
        cwd,
        Source::Github(info),
        Some(&repository),
    ))
}
