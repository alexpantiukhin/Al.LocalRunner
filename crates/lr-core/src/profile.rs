use crate::error::ProfileError;
use crate::model::{StepState, Zone};
use crate::profile_args::ArgSpec;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const SINGLE_STEP_NAME: &str = "run";

/// A program a job needs on the runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Program {
    /// Program name, looked up in the runner's `PATH`.
    pub name: String,
    /// Optional command that must exit with code 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
}

/// One step of a profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepSpec {
    /// Step name.
    pub name: String,
    /// Command line, not interpreted by the core.
    pub command: String,
    /// Mandatory step timeout in seconds.
    pub timeout_s: u64,
    /// Overrides the profile memory for this step.
    pub mem_mb: Option<u64>,
    /// Overrides the profile CPU count for this step.
    pub cpu: Option<u32>,
    /// Tags added to the profile tags for this step.
    pub tags: Option<Vec<String>>,
    /// Secrets requested by this step only.
    pub secrets: Vec<String>,
    /// Keep going after this step fails; the job result stays failed.
    pub continue_on_failure: bool,
}

impl StepSpec {
    /// The timeout the step is killed by: the earlier of its own timeout and the
    /// time left of the job timeout.
    pub fn effective_timeout_s(&self, job_remaining_s: u64) -> u64 {
        self.timeout_s.min(job_remaining_s)
    }
}

/// `[profile.X.platform.P]` override.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PlatformOverride {
    /// Replaces the command of the profile (one step).
    pub command: Option<String>,
    /// Replaces the steps of the profile.
    pub steps: Option<Vec<StepSpec>>,
    /// Replaces the job timeout.
    pub timeout_s: Option<u64>,
    /// Replaces the memory requirement.
    pub mem_mb: Option<u64>,
    /// Replaces the CPU requirement.
    pub cpu: Option<u32>,
    /// Replaces the disk requirement.
    pub disk_mb: Option<u64>,
}

fn yes() -> bool {
    true
}

/// The steps of one zone of a profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneSteps {
    /// Ordered steps of the zone.
    pub steps: Vec<StepSpec>,
    /// Whether they came from a single `command`.
    pub from_command: bool,
}

/// A validated profile: the requirements and steps of a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Profile name.
    pub name: String,
    /// Platforms the profile can run on; empty means any.
    pub platforms: Vec<String>,
    /// Mandatory job timeout in seconds.
    pub timeout_s: u64,
    /// Memory in MB.
    pub mem_mb: Option<u64>,
    /// CPU count.
    pub cpu: Option<u32>,
    /// Disk in MB.
    pub disk_mb: Option<u64>,
    /// Programs needed on the runner.
    pub programs: Vec<Program>,
    /// Free-form labels the policy can match; not requirements, sorted and unique.
    pub labels: Vec<String>,
    /// Resources held for the run.
    pub tags: Vec<String>,
    /// Tags nobody else may hold meanwhile.
    pub exclusive: Vec<String>,
    /// Named caches kept between runs.
    pub cache: Vec<String>,
    /// Runner services needed.
    pub services: Vec<String>,
    /// Secret names needed by the whole job.
    pub secrets: Vec<String>,
    /// Zones the profile may run in; `None` allows every zone. With `zoned` they are narrowed
    /// to the zones that have steps; the legacy `zone` key gives the zones up to it.
    pub zones: Option<BTreeSet<Zone>>,
    /// Whether the profile uses zones for its steps: it has zone variants or a `zones` list.
    /// Such an order is not tied to the client's machine and its steps come per zone.
    #[serde(default)]
    pub zoned: bool,
    /// Ordered steps; a single `command` is one step. When the profile has no common block
    /// of its own (`common` is false) these are the steps of its lowest zone variant, kept
    /// for the step rows every job has.
    pub steps: Vec<StepSpec>,
    /// Whether `steps` came from a single `command`.
    pub from_command: bool,
    /// Whether the profile has its own `command` or `steps`, which serve the zones `local`
    /// and `shared` that have no variant of their own.
    #[serde(default = "yes")]
    pub common: bool,
    /// `[profile.X.zone.<zone>]`: the steps of a zone, replacing the common ones there.
    #[serde(default)]
    pub zone_steps: BTreeMap<Zone, ZoneSteps>,
    /// Per-platform overrides.
    pub platform_overrides: BTreeMap<String, PlatformOverride>,
    /// Named arguments the submitter fills into the commands.
    #[serde(default)]
    pub args: Vec<ArgSpec>,
}

impl Profile {
    /// The steps each zone runs: its own variant, else the common block for `local` and
    /// `shared`. A zone missing from the map cannot run the profile: `lan` and `remote` get
    /// code only from a variant of their own.
    pub fn zone_steps_effective(&self) -> BTreeMap<Zone, Vec<StepSpec>> {
        let mut out = BTreeMap::new();
        for zone in Zone::ALL {
            if let Some(v) = self.zone_steps.get(&zone) {
                out.insert(zone, v.steps.clone());
            } else if self.common && matches!(zone, Zone::Local | Zone::Shared) {
                out.insert(zone, self.steps.clone());
            }
        }
        out
    }

    /// Indexes of the steps skipped when step `failed` fails; empty when the
    /// failed step has `continue_on_failure`.
    pub fn skipped_after_failure(&self, failed: usize) -> Vec<usize> {
        match self.steps.get(failed) {
            Some(step) if !step.continue_on_failure => ((failed + 1)..self.steps.len()).collect(),
            _ => Vec::new(),
        }
    }

    /// Step states right after step `failed` fails.
    pub fn states_after_failure(&self, failed: usize) -> Vec<StepState> {
        let skipped = self.skipped_after_failure(failed);
        (0..self.steps.len())
            .map(|i| {
                if i < failed {
                    StepState::Passed
                } else if i == failed {
                    StepState::Failed
                } else if skipped.contains(&i) {
                    StepState::Skipped
                } else {
                    StepState::Queued
                }
            })
            .collect()
    }
}

/// Applies the `[profile.X.platform.P]` override for `platform`; fails when the
/// profile lists platforms and `platform` is not among them.
pub fn resolve_for_platform(p: &Profile, platform: &str) -> Result<Profile, ProfileError> {
    let listed = !p.platforms.is_empty() || !p.platform_overrides.is_empty();
    let known =
        p.platforms.iter().any(|x| x == platform) || p.platform_overrides.contains_key(platform);
    if listed && !known {
        return Err(ProfileError::UnsupportedPlatform {
            profile: p.name.clone(),
            platform: platform.to_string(),
        });
    }
    let mut out = p.clone();
    out.platforms = vec![platform.to_string()];
    out.platform_overrides.clear();
    let Some(over) = p.platform_overrides.get(platform) else {
        return Ok(out);
    };
    let timeout_before = out.timeout_s;
    if let Some(t) = over.timeout_s {
        out.timeout_s = t;
    }
    if over.mem_mb.is_some() {
        out.mem_mb = over.mem_mb;
    }
    if over.cpu.is_some() {
        out.cpu = over.cpu;
    }
    if over.disk_mb.is_some() {
        out.disk_mb = over.disk_mb;
    }
    if let Some(command) = &over.command {
        out.steps = vec![single_step(command.clone(), out.timeout_s)];
        out.from_command = true;
    } else if let Some(steps) = &over.steps {
        out.steps = steps.clone();
        out.from_command = false;
    } else if out.from_command {
        out.steps[0].timeout_s = out.timeout_s;
    }
    if out.timeout_s != timeout_before {
        let timeout_s = out.timeout_s;
        for v in out.zone_steps.values_mut().filter(|v| v.from_command) {
            v.steps[0].timeout_s = timeout_s;
        }
    }
    Ok(out)
}

pub(crate) fn single_step(command: String, timeout_s: u64) -> StepSpec {
    StepSpec {
        name: SINGLE_STEP_NAME.to_string(),
        command,
        timeout_s,
        mem_mb: None,
        cpu: None,
        tags: None,
        secrets: Vec::new(),
        continue_on_failure: false,
    }
}
