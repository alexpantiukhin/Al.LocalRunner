use crate::error::ProfileError;
use crate::model::Zone;
use crate::profile_args::{build_args, RawArg};
use crate::profile::{single_step, PlatformOverride, Profile, Program, StepSpec};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct RawFile {
    #[serde(default)]
    profile: BTreeMap<String, toml::Value>,
    #[serde(default)]
    needs: BTreeMap<String, toml::Value>,
}

#[derive(Deserialize, Clone)]
#[serde(untagged)]
enum RawProgram {
    Name(String),
    Full { name: String, check: Option<String> },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    name: String,
    command: String,
    timeout_s: Option<u64>,
    mem_mb: Option<u64>,
    cpu: Option<u32>,
    tags: Option<Vec<String>>,
    #[serde(default)]
    secrets: Vec<String>,
    #[serde(default)]
    continue_on_failure: bool,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawOverride {
    command: Option<String>,
    steps: Option<Vec<RawStep>>,
    timeout_s: Option<u64>,
    mem_mb: Option<u64>,
    cpu: Option<u32>,
    disk_mb: Option<u64>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
struct RawNeeds {
    timeout_s: Option<u64>,
    mem_mb: Option<u64>,
    cpu: Option<u32>,
    disk_mb: Option<u64>,
    programs: Option<Vec<RawProgram>>,
    tags: Option<Vec<String>>,
    exclusive: Option<Vec<String>>,
    cache: Option<Vec<String>>,
    services: Option<Vec<String>>,
    secrets: Option<Vec<String>>,
    zone: Option<String>,
    platforms: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    needs: Option<String>,
    command: Option<String>,
    steps: Option<Vec<RawStep>>,
    timeout_s: Option<u64>,
    mem_mb: Option<u64>,
    cpu: Option<u32>,
    disk_mb: Option<u64>,
    programs: Option<Vec<RawProgram>>,
    tags: Option<Vec<String>>,
    exclusive: Option<Vec<String>>,
    cache: Option<Vec<String>>,
    services: Option<Vec<String>>,
    secrets: Option<Vec<String>>,
    zone: Option<String>,
    platforms: Option<Vec<String>>,
    #[serde(default)]
    platform: BTreeMap<String, RawOverride>,
    #[serde(default)]
    args: BTreeMap<String, RawArg>,
}

impl RawNeeds {
    fn over(self, base: &RawNeeds) -> RawNeeds {
        let base = base.clone();
        RawNeeds {
            timeout_s: self.timeout_s.or(base.timeout_s),
            mem_mb: self.mem_mb.or(base.mem_mb),
            cpu: self.cpu.or(base.cpu),
            disk_mb: self.disk_mb.or(base.disk_mb),
            programs: self.programs.or(base.programs),
            tags: self.tags.or(base.tags),
            exclusive: self.exclusive.or(base.exclusive),
            cache: self.cache.or(base.cache),
            services: self.services.or(base.services),
            secrets: self.secrets.or(base.secrets),
            zone: self.zone.or(base.zone),
            platforms: self.platforms.or(base.platforms),
        }
    }
}

/// Parses `.localrunner/profiles.toml` into validated profiles ordered by name.
pub fn parse_profiles(text: &str) -> Result<Vec<Profile>, ProfileError> {
    let file: RawFile = toml::from_str(text).map_err(|e| ProfileError::Syntax(e.to_string()))?;
    let mut sets = BTreeMap::new();
    for (name, value) in file.needs {
        let raw: RawNeeds = value.try_into().map_err(|e: toml::de::Error| {
            ProfileError::invalid(&format!("needs.{name}"), &error_key(&e), e.to_string())
        })?;
        sets.insert(name, raw);
    }
    file.profile
        .into_iter()
        .map(|(name, value)| {
            let raw: RawProfile = value
                .try_into()
                .map_err(|e: toml::de::Error| ProfileError::invalid(&name, "", e.to_string()))?;
            build(name, raw, &sets)
        })
        .collect()
}

fn build(
    name: String,
    raw: RawProfile,
    sets: &BTreeMap<String, RawNeeds>,
) -> Result<Profile, ProfileError> {
    let own = RawNeeds {
        timeout_s: raw.timeout_s,
        mem_mb: raw.mem_mb,
        cpu: raw.cpu,
        disk_mb: raw.disk_mb,
        programs: raw.programs,
        tags: raw.tags,
        exclusive: raw.exclusive,
        cache: raw.cache,
        services: raw.services,
        secrets: raw.secrets,
        zone: raw.zone,
        platforms: raw.platforms,
    };
    let needs = match &raw.needs {
        Some(set) => {
            let base = sets.get(set).ok_or_else(|| {
                ProfileError::invalid(&name, "needs", format!("unknown needs set `{set}`"))
            })?;
            own.over(base)
        }
        None => own,
    };
    let timeout_s = positive(&name, "timeout_s", needs.timeout_s)?
        .ok_or_else(|| ProfileError::invalid(&name, "timeout_s", "required"))?;
    let (steps, from_command) = match (raw.command, raw.steps) {
        (Some(_), Some(_)) => {
            return Err(ProfileError::invalid(
                &name,
                "command",
                "`command` and `steps` are mutually exclusive",
            ))
        }
        (None, None) => {
            return Err(ProfileError::invalid(
                &name,
                "command",
                "either `command` or `steps` is required",
            ))
        }
        (Some(c), None) => (
            vec![single_step(non_empty(&name, "command", c)?, timeout_s)],
            true,
        ),
        (None, Some(s)) => (build_steps(&name, "steps", s)?, false),
    };
    let zone = needs
        .zone
        .map(|z| {
            z.parse::<Zone>()
                .map_err(|r| ProfileError::invalid(&name, "zone", r))
        })
        .transpose()?;
    let mut overrides = BTreeMap::new();
    for (platform, o) in raw.platform {
        let field = format!("platform.{platform}");
        overrides.insert(platform, build_override(&name, &field, o)?);
    }
    let args = build_args(&name, raw.args)?;
    Ok(Profile {
        platforms: needs.platforms.unwrap_or_default(),
        timeout_s,
        mem_mb: needs.mem_mb,
        cpu: needs.cpu,
        disk_mb: needs.disk_mb,
        programs: needs
            .programs
            .unwrap_or_default()
            .into_iter()
            .map(program)
            .collect(),
        tags: needs.tags.unwrap_or_default(),
        exclusive: needs.exclusive.unwrap_or_default(),
        cache: needs.cache.unwrap_or_default(),
        services: needs.services.unwrap_or_default(),
        secrets: needs.secrets.unwrap_or_default(),
        zone,
        steps,
        from_command,
        platform_overrides: overrides,
        args,
        name,
    })
}

fn program(raw: RawProgram) -> Program {
    match raw {
        RawProgram::Name(name) => Program { name, check: None },
        RawProgram::Full { name, check } => Program { name, check },
    }
}

fn positive(profile: &str, field: &str, v: Option<u64>) -> Result<Option<u64>, ProfileError> {
    match v {
        Some(0) => Err(ProfileError::invalid(
            profile,
            field,
            "must be greater than zero",
        )),
        other => Ok(other),
    }
}

fn non_empty(profile: &str, field: &str, v: String) -> Result<String, ProfileError> {
    if v.trim().is_empty() {
        Err(ProfileError::invalid(profile, field, "must not be empty"))
    } else {
        Ok(v)
    }
}

fn build_override(
    profile: &str,
    field: &str,
    o: RawOverride,
) -> Result<PlatformOverride, ProfileError> {
    if o.command.is_some() && o.steps.is_some() {
        return Err(ProfileError::invalid(
            profile,
            &format!("{field}.command"),
            "`command` and `steps` are mutually exclusive",
        ));
    }
    Ok(PlatformOverride {
        command: o
            .command
            .map(|c| non_empty(profile, &format!("{field}.command"), c))
            .transpose()?,
        steps: o
            .steps
            .map(|s| build_steps(profile, &format!("{field}.steps"), s))
            .transpose()?,
        timeout_s: positive(profile, &format!("{field}.timeout_s"), o.timeout_s)?,
        mem_mb: o.mem_mb,
        cpu: o.cpu,
        disk_mb: o.disk_mb,
    })
}

fn build_steps(
    profile: &str,
    field: &str,
    raw: Vec<RawStep>,
) -> Result<Vec<StepSpec>, ProfileError> {
    if raw.is_empty() {
        return Err(ProfileError::invalid(
            profile,
            field,
            "must contain at least one step",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for (i, s) in raw.iter().enumerate() {
        if !seen.insert(s.name.clone()) {
            return Err(ProfileError::invalid(
                profile,
                &format!("{field}[{i}].name"),
                format!("duplicate step name `{}`", s.name),
            ));
        }
    }
    raw.into_iter()
        .enumerate()
        .map(|(i, s)| {
            let at = format!("{field}[{i}]");
            let timeout_s = positive(profile, &format!("{at}.timeout_s"), s.timeout_s)?
                .ok_or_else(|| {
                    ProfileError::invalid(profile, &format!("{at}.timeout_s"), "required")
                })?;
            Ok(StepSpec {
                name: s.name,
                command: non_empty(profile, &format!("{at}.command"), s.command)?,
                timeout_s,
                mem_mb: s.mem_mb,
                cpu: s.cpu,
                tags: s.tags,
                secrets: s.secrets,
                continue_on_failure: s.continue_on_failure,
            })
        })
        .collect()
}

fn error_key(e: &toml::de::Error) -> String {
    let msg = e.message();
    msg.split('`')
        .nth(1)
        .map(str::to_string)
        .unwrap_or_default()
}
