use crate::error::ProfileError;
use crate::model::Zone;
use crate::profile::{resolve_for_platform, Profile, Program};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Opaque reference to the machine of the client, assigned by the broker from
/// the local IPC peer; a runner holding an equal reference is "the same machine".
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MachineRef(pub String);

/// What a job needs from a runner (spec 4), derived from a profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "WireRequirements")]
pub struct Requirements {
    /// The single platform of the job; `None` for a same-machine job.
    pub platform: Option<String>,
    /// Job timeout in seconds.
    pub timeout_s: u64,
    /// Memory in MB: the maximum over steps.
    pub mem_mb: u64,
    /// CPU count: the maximum over steps.
    pub cpu: u32,
    /// Disk in MB.
    pub disk_mb: u64,
    /// Programs that must exist on the runner.
    pub programs: Vec<Program>,
    /// Tags held for the run: the union over profile and steps.
    pub tags: BTreeSet<String>,
    /// Tags nobody else may hold meanwhile.
    pub exclusive: BTreeSet<String>,
    /// Set when the job must run on the same machine as the client.
    pub same_machine: Option<MachineRef>,
    /// Services the runner must offer.
    pub services: Vec<String>,
    /// Secret names requested by the job or any step.
    pub secrets: BTreeSet<String>,
    /// Named caches.
    pub cache: Vec<String>,
    /// Zones a runner may be in relative to the client; `None` allows every zone. The profile
    /// and the order's `--zones` already narrowed it to the zones the profile has steps for.
    pub zones: Option<BTreeSet<Zone>>,
    /// Command names the runner must declare: the broker fills them from the steps of a client
    /// limited to named commands (`[commands] mode = "named"`, spec 3.5) and empties them for any
    /// other client; absent from senders of an earlier minor version.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub commands: BTreeSet<String>,
}

/// [`Requirements`] as it is read: a record written before zones were a collection carries the
/// farthest allowed zone as `max_zone`, which becomes the zones from the nearest up to it.
#[derive(Deserialize)]
struct WireRequirements {
    platform: Option<String>,
    timeout_s: u64,
    mem_mb: u64,
    cpu: u32,
    disk_mb: u64,
    programs: Vec<Program>,
    tags: BTreeSet<String>,
    exclusive: BTreeSet<String>,
    same_machine: Option<MachineRef>,
    services: Vec<String>,
    secrets: BTreeSet<String>,
    cache: Vec<String>,
    #[serde(default)]
    zones: Option<BTreeSet<Zone>>,
    #[serde(default)]
    max_zone: Option<Zone>,
    #[serde(default)]
    commands: BTreeSet<String>,
}

impl From<WireRequirements> for Requirements {
    fn from(w: WireRequirements) -> Requirements {
        Requirements {
            platform: w.platform,
            timeout_s: w.timeout_s,
            mem_mb: w.mem_mb,
            cpu: w.cpu,
            disk_mb: w.disk_mb,
            programs: w.programs,
            tags: w.tags,
            exclusive: w.exclusive,
            same_machine: w.same_machine,
            services: w.services,
            secrets: w.secrets,
            cache: w.cache,
            zones: w.zones.or_else(|| w.max_zone.map(Zone::up_to)),
            commands: w.commands,
        }
    }
}

impl Requirements {
    /// Derives the requirements of a job from `profile`.
    ///
    /// With a `platform` the profile is resolved for it and the job is not tied to
    /// a machine. Without one the job runs on the client's machine: `client_machine`
    /// becomes `same_machine` and is required: without it an error is returned.
    pub fn from_profile(
        profile: &Profile,
        platform: Option<&str>,
        client_machine: Option<MachineRef>,
    ) -> Result<Requirements, ProfileError> {
        if platform.is_none() && client_machine.is_none() {
            return Err(ProfileError::Invalid {
                profile: profile.name.clone(),
                field: "platform".into(),
                reason: "a job without a platform needs the client machine".into(),
            });
        }
        let p = match platform {
            Some(name) => resolve_for_platform(profile, name)?,
            None => profile.clone(),
        };
        let zone_steps = p.zone_steps_effective();
        let zones = if p.zoned {
            let mut zones: BTreeSet<Zone> = zone_steps.keys().copied().collect();
            if let Some(allowed) = &p.zones {
                zones.retain(|z| allowed.contains(z));
            }
            if zones.is_empty() {
                return Err(ProfileError::Invalid {
                    profile: profile.name.clone(),
                    field: "zones".into(),
                    reason: "no zone is both allowed and has steps".into(),
                });
            }
            Some(zones)
        } else {
            p.zones.clone()
        };
        let mut mem_mb = 0;
        let mut cpu = 0;
        let mut tags: BTreeSet<String> = p.tags.iter().cloned().collect();
        let mut secrets: BTreeSet<String> = p.secrets.iter().cloned().collect();
        let allowed = |z: &Zone| zones.as_ref().is_none_or(|list| list.contains(z));
        for step in zone_steps
            .iter()
            .filter(|(z, _)| allowed(z))
            .flat_map(|(_, steps)| steps)
        {
            mem_mb = mem_mb.max(step.mem_mb.or(p.mem_mb).unwrap_or(0));
            cpu = cpu.max(step.cpu.or(p.cpu).unwrap_or(0));
            tags.extend(step.tags.iter().flatten().cloned());
            secrets.extend(step.secrets.iter().cloned());
        }
        let mut programs: Vec<Program> = Vec::new();
        for program in &p.programs {
            if !programs.iter().any(|x| x.name == program.name) {
                programs.push(program.clone());
            }
        }
        Ok(Requirements {
            platform: platform.map(str::to_string),
            timeout_s: p.timeout_s,
            mem_mb,
            cpu,
            disk_mb: p.disk_mb.unwrap_or(0),
            programs,
            tags,
            exclusive: p.exclusive.iter().cloned().collect(),
            same_machine: if platform.is_none() {
                client_machine
            } else {
                None
            },
            services: p.services.clone(),
            secrets,
            cache: p.cache.clone(),
            zones,
            commands: BTreeSet::new(),
        })
    }
}

/// Name of the environment variable that tells a job the path of cache `name`.
pub fn cache_env_var(name: &str) -> String {
    let upper: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("LOCALRUNNER_CACHE_{upper}")
}
