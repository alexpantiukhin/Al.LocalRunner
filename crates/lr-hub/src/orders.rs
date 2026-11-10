use crate::handles::checked_dir;
use lr_abstractions::{Files, Frame, Repository, MAX_FRAME_BYTES};
use lr_core::{
    parse_profiles, resolve_for_platform, validate_label, JobId, MachineRef, Profile, Requirements,
    Timestamp,
};
use lr_proto::{HubToBroker, Order, OrderSpec, ReasonCode, RejectReason, SubmitRequest, WireCode};
use std::path::{Path, PathBuf};

/// Path of the profiles inside a work tree or a commit.
pub(crate) const PROFILES_PATH: &str = ".localrunner/profiles.toml";

/// Why a submit produced no order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    Rejected(ReasonCode, String),
    Invalid(String),
}

fn invalid(text: impl Into<String>) -> Refusal {
    Refusal::Invalid(text.into())
}

/// An order built from a submit, ready for the outbox.
pub(crate) struct Built {
    pub(crate) spec: OrderSpec,
}

/// The reference "same machine" of a local IPC client as the broker assigns it (spec 13.2).
pub(crate) fn local_machine(os_user: &str) -> MachineRef {
    MachineRef(format!("local:{os_user}"))
}

fn pick(profiles: Vec<Profile>, wanted: Option<&str>, origin: &str) -> Result<Profile, Refusal> {
    if let Some(name) = wanted {
        return profiles
            .into_iter()
            .find(|p| p.name == name)
            .ok_or_else(|| invalid(format!("profile `{name}` is not in {origin}")));
    }
    let names: Vec<String> = profiles.iter().map(|p| p.name.clone()).collect();
    let mut all = profiles.into_iter();
    match (all.next(), all.next()) {
        (Some(only), None) => Ok(only),
        _ => Err(invalid(format!(
            "{origin} has {} profiles; name one: {}",
            names.len(),
            names.join(", ")
        ))),
    }
}

fn parse(bytes: &[u8], origin: &str) -> Result<Vec<Profile>, Refusal> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid(format!("{origin} is not UTF-8")))?;
    parse_profiles(text).map_err(|e| invalid(format!("{origin}: {e}")))
}

fn platform_of(profile: &Profile, flag: Option<&str>) -> Result<Option<String>, Refusal> {
    if let Some(p) = flag {
        return Ok(Some(p.to_string()));
    }
    let mut listed: Vec<&String> = profile
        .platforms
        .iter()
        .chain(profile.platform_overrides.keys())
        .collect();
    listed.sort();
    listed.dedup();
    match listed.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some((*one).clone())),
        many => Err(invalid(format!(
            "profile `{}` lists several platforms ({}); choose one with --platform",
            profile.name,
            many.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

async fn committed(
    repo: &dyn Repository,
    root: &Path,
    name: &str,
) -> Result<(String, Profile, Option<String>), Refusal> {
    let repo_err = |e: lr_abstractions::RepositoryError| invalid(e.to_string());
    let sha = repo
        .head(root)
        .await
        .map_err(repo_err)?
        .ok_or_else(|| invalid(format!("{} has no commit", root.display())))?;
    if !repo.is_pushed(root, &sha).await.map_err(repo_err)? {
        return Err(Refusal::Rejected(
            ReasonCode::Rejected(RejectReason::CommitNotPushed),
            format!("HEAD {sha} не запушен: закоммитьте и запушьте ветку (MR не нужен)"),
        ));
    }
    let origin = format!("{PROFILES_PATH} of commit {sha}");
    let bytes = repo
        .show(root, &sha, PROFILES_PATH)
        .await
        .map_err(repo_err)?
        .ok_or_else(|| invalid(format!("{origin} does not exist")))?;
    let profile = pick(parse(&bytes, &origin)?, Some(name), &origin)?;
    let url = repo.origin_url(root).await.map_err(repo_err)?;
    Ok((sha, profile, url))
}

fn lower_timeout(
    spec: &mut OrderSpec,
    profile_timeout: u64,
    flag: Option<u64>,
) -> Result<(), Refusal> {
    let Some(t) = flag else {
        return Ok(());
    };
    if t == 0 || t > profile_timeout {
        return Err(invalid(format!(
            "--timeout {t} must be between 1 and the profile timeout {profile_timeout} s: it only lowers"
        )));
    }
    spec.requirements.timeout_s = t;
    for step in &mut spec.steps {
        step.timeout_s = step.timeout_s.min(t);
    }
    Ok(())
}

/// Reads the profile (work tree, or the pushed commit for a platform order) and builds the
/// order (spec 3.4, 3.7, 5).
pub(crate) async fn build(
    req: &SubmitRequest,
    files: &dyn Files,
    repo: &dyn Repository,
    now: Timestamp,
) -> Result<Built, Refusal> {
    let id = &req.identity;
    validate_label(&id.label).map_err(|e| invalid(e.to_string()))?;
    checked_dir(&id.cwd).map_err(invalid)?;
    let root: PathBuf = match &id.worktree_root {
        Some(r) => {
            checked_dir(r).map_err(invalid)?;
            r.clone()
        }
        None => id.cwd.clone(),
    };
    let path = root.join(".localrunner").join("profiles.toml");
    let origin = path.display().to_string();
    let bytes = files
        .read(&path)
        .await
        .map_err(|e| invalid(e.to_string()))?
        .ok_or_else(|| invalid(format!("{origin} does not exist")))?;
    let tree_profile = pick(parse(&bytes, &origin)?, req.profile.as_deref(), &origin)?;
    let platform = platform_of(&tree_profile, req.platform.as_deref())?;
    let (profile, code) = match &platform {
        None => (
            tree_profile,
            WireCode::WorkTree {
                cwd: id.cwd.display().to_string(),
            },
        ),
        Some(_) => {
            let (sha, profile, repo_url) = committed(repo, &root, &tree_profile.name).await?;
            (
                profile,
                WireCode::Commit {
                    sha,
                    repo_url,
                    branch: id.branch.clone(),
                },
            )
        }
    };
    let requirements = Requirements::from_profile(
        &profile,
        platform.as_deref(),
        Some(local_machine(&id.os_user)),
    )
    .map_err(|e| invalid(e.to_string()))?;
    let resolved = match &platform {
        Some(p) => resolve_for_platform(&profile, p).map_err(|e| invalid(e.to_string()))?,
        None => profile.clone(),
    };
    let mut spec = OrderSpec {
        profile: profile.name.clone(),
        requirements,
        steps: resolved.steps,
        enqueued_at: now,
        identity: id.clone(),
        code,
        wanted_priority: None,
    };
    lower_timeout(&mut spec, resolved.timeout_s, req.timeout_s)?;
    let probe = order_payload(&JobId("probe".into()), &spec);
    if Frame::json(&probe).map_or(true, |f| f.len() > MAX_FRAME_BYTES / 2) {
        return Err(invalid("the order is too large for one protocol frame"));
    }
    Ok(Built { spec })
}

/// The order message as stored in the outbox; its seq is set when it is sent.
pub(crate) fn order_payload(job: &JobId, spec: &OrderSpec) -> HubToBroker {
    HubToBroker::Order(Order {
        job: job.clone(),
        seq: 0,
        spec: Box::new(spec.clone()),
    })
}

/// Issues job ids: hub time in milliseconds and a counter, monotonic even if the clock
/// stands still, so an id is never reused by this hub.
#[derive(Default)]
pub(crate) struct JobIds {
    last_ms: u64,
    counter: u32,
}

impl JobIds {
    pub(crate) fn next(&mut self, now: Timestamp) -> JobId {
        if now.0 > self.last_ms {
            self.last_ms = now.0;
            self.counter = 0;
        } else {
            self.counter = self.counter.wrapping_add(1);
            if self.counter == 0 {
                self.last_ms = self.last_ms.saturating_add(1);
            }
        }
        JobId(format!("j{:x}-{:x}", self.last_ms, self.counter))
    }
}
