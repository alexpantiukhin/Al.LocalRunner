//! `profile check`: the per-profile summary and its rendering.

use lr_core::{preview_args, Profile, StepSpec};
use serde::Serialize;
use std::collections::BTreeSet;

/// One step of a profile as shown by `profile check`.
#[derive(Debug, Clone, Serialize)]
pub struct StepSummary {
    /// Step name.
    pub name: String,
    /// Command line of the step.
    pub command: String,
    /// Step timeout in seconds.
    pub timeout_s: u64,
}

/// The steps a zone runs when the profile has a variant for it.
#[derive(Debug, Clone, Serialize)]
pub struct ZoneSummary {
    /// The zone name.
    pub zone: String,
    /// The single command of the variant; empty when it has `steps`.
    pub command: Option<String>,
    /// The steps of the variant; empty when it has a single command.
    pub steps: Vec<StepSummary>,
}

/// A variant of a profile selected by a filter over the runner.
#[derive(Debug, Clone, Serialize)]
pub struct VariantSummary {
    /// The name of the variant.
    pub name: String,
    /// The filter as `key a, b; key c`, empty for a variant that matches every runner.
    pub when: String,
    /// The single command of the variant.
    pub command: Option<String>,
    /// The steps of the variant; empty when it has a single command.
    pub steps: Vec<StepSummary>,
}

/// One profile as shown by `profile check`.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileSummary {
    /// Profile name.
    pub name: String,
    /// Job timeout in seconds.
    pub timeout_s: u64,
    /// The single command, when the profile has `command` instead of `steps`.
    pub command: Option<String>,
    /// The steps; empty when the profile has a single command.
    pub steps: Vec<StepSummary>,
    /// Memory in MB.
    pub mem_mb: Option<u64>,
    /// CPU count.
    pub cpu: Option<u32>,
    /// Disk in MB.
    pub disk_mb: Option<u64>,
    /// `"with platforms"` when the profile has platforms or platform overrides, else `"no platforms"`.
    pub platform_mode: &'static str,
    /// The platforms and the override keys, sorted.
    pub platforms: Vec<String>,
    /// The zone variants, nearest zone first.
    pub zones: Vec<ZoneSummary>,
    /// The variants selected by a filter, in the order they are tried.
    pub variants: Vec<VariantSummary>,
    /// Tags held for the run.
    pub tags: Vec<String>,
    /// Tags nobody else may hold meanwhile.
    pub exclusive: Vec<String>,
    /// Programs the runner must declare.
    pub programs: Vec<String>,
    /// Zones the profile may run in, when it limits them.
    pub allowed_zones: Vec<String>,
    /// Names of the arguments.
    pub args: Vec<String>,
    /// Labels of the profile, sorted; the policy can match them.
    pub labels: Vec<String>,
    /// The commands the profile expands to with every argument given and with none given;
    /// empty unless a command has an optional segment `{? ...}`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub expansions: Vec<Expansion>,
}

/// The commands of a profile for one set of argument values.
#[derive(Debug, Clone, Serialize)]
pub struct Expansion {
    /// Which values were given: `"all arguments given"` or `"no argument given"`.
    pub case: &'static str,
    /// The expanded steps; empty when the values do not bind.
    pub steps: Vec<StepSummary>,
    /// Why the values do not bind, for example a required argument that is missing.
    pub error: Option<String>,
}

fn expand(p: &Profile, case: &'static str, all_given: bool) -> Expansion {
    let mut steps = p.steps.clone();
    match preview_args(&mut steps, &p.args, all_given) {
        Ok(()) => Expansion {
            case,
            steps: steps.iter().map(step).collect(),
            error: None,
        },
        Err(e) => Expansion {
            case,
            steps: Vec::new(),
            error: Some(e.to_string()),
        },
    }
}

fn expansions(p: &Profile) -> Vec<Expansion> {
    if p.args.is_empty() || !p.steps.iter().any(|s| s.command.contains("{?")) {
        return Vec::new();
    }
    vec![
        expand(p, "all arguments given", true),
        expand(p, "no argument given", false),
    ]
}

fn step(s: &StepSpec) -> StepSummary {
    StepSummary {
        name: s.name.clone(),
        command: s.command.clone(),
        timeout_s: s.timeout_s,
    }
}

/// Builds the summary of a resolved profile.
pub fn summarize(p: &Profile) -> ProfileSummary {
    let platforms: Vec<String> = p
        .platforms
        .iter()
        .chain(p.platform_overrides.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let command = p
        .from_command
        .then(|| p.steps.first().map(|s| s.command.clone()))
        .flatten();
    ProfileSummary {
        name: p.name.clone(),
        timeout_s: p.timeout_s,
        steps: if p.from_command {
            Vec::new()
        } else {
            p.steps.iter().map(step).collect()
        },
        command,
        mem_mb: p.mem_mb,
        cpu: p.cpu,
        disk_mb: p.disk_mb,
        platform_mode: if platforms.is_empty() {
            "no platforms"
        } else {
            "with platforms"
        },
        platforms,
        zones: p
            .zone_steps
            .iter()
            .map(|(zone, v)| ZoneSummary {
                zone: zone.to_string(),
                command: v
                    .from_command
                    .then(|| v.steps.first().map(|s| s.command.clone()))
                    .flatten(),
                steps: if v.from_command {
                    Vec::new()
                } else {
                    v.steps.iter().map(step).collect()
                },
            })
            .collect(),
        variants: p
            .variants
            .iter()
            .map(|v| {
                let w = &v.when;
                let zones: Vec<String> = w.zones.iter().map(ToString::to_string).collect();
                let when = [
                    ("zone", &zones),
                    ("os", &w.os),
                    ("tags", &w.tags),
                    ("programs", &w.programs),
                    ("runner", &w.runners),
                ]
                .iter()
                .filter(|(_, items)| !items.is_empty())
                .map(|(key, items)| format!("{key} {}", items.join(", ")))
                .collect::<Vec<_>>()
                .join("; ");
                VariantSummary {
                    name: v.name.clone(),
                    when,
                    command: v
                        .from_command
                        .then(|| v.steps.first().map(|s| s.command.clone()))
                        .flatten(),
                    steps: if v.from_command {
                        Vec::new()
                    } else {
                        v.steps.iter().map(step).collect()
                    },
                }
            })
            .collect(),
        tags: p.tags.clone(),
        exclusive: p.exclusive.clone(),
        programs: p.programs.iter().map(|x| x.name.clone()).collect(),
        allowed_zones: p
            .zones
            .as_ref()
            .map(|z| z.iter().map(ToString::to_string).collect())
            .unwrap_or_default(),
        args: p.args.iter().map(|a| a.name.clone()).collect(),
        labels: p.labels.clone(),
        expansions: expansions(p),
    }
}

/// The text lines of the summary, one block per profile.
pub fn summary_lines(list: &[ProfileSummary]) -> Vec<String> {
    let mut out = Vec::new();
    for p in list {
        let platforms = if p.platforms.is_empty() {
            String::new()
        } else {
            format!(" ({})", p.platforms.join(", "))
        };
        out.push(format!(
            "profile {}: {}{platforms}, timeout {} s",
            p.name, p.platform_mode, p.timeout_s
        ));
        if let Some(c) = &p.command {
            out.push(format!("  command: {c}"));
        }
        for s in &p.steps {
            out.push(format!(
                "  step {}: {} (timeout {} s)",
                s.name, s.command, s.timeout_s
            ));
        }
        for z in &p.zones {
            if let Some(c) = &z.command {
                out.push(format!("  zone {}: {c}", z.zone));
            }
            for s in &z.steps {
                out.push(format!(
                    "  zone {}, step {}: {} (timeout {} s)",
                    z.zone, s.name, s.command, s.timeout_s
                ));
            }
        }
        for v in &p.variants {
            let filter = if v.when.is_empty() {
                String::new()
            } else {
                format!(" ({})", v.when)
            };
            if let Some(c) = &v.command {
                out.push(format!("  variant {}{filter}: {c}", v.name));
            }
            for s in &v.steps {
                out.push(format!(
                    "  variant {}{filter}, step {}: {} (timeout {} s)",
                    v.name, s.name, s.command, s.timeout_s
                ));
            }
        }
        let list = |label: &str, v: &[String]| {
            (!v.is_empty()).then(|| format!("{label} {}", v.join(", ")))
        };
        let parts: Vec<String> = [
            p.mem_mb.map(|v| format!("mem {v} MB")),
            p.cpu.map(|v| format!("cpu {v}")),
            p.disk_mb.map(|v| format!("disk {v} MB")),
            list("tags", &p.tags),
            list("exclusive", &p.exclusive),
            list("programs", &p.programs),
            list("zones", &p.allowed_zones),
        ]
        .into_iter()
        .flatten()
        .collect();
        if parts.is_empty() {
            out.push("  resources: none declared".into());
        } else {
            out.push(format!("  resources: {}", parts.join(", ")));
        }
        if !p.args.is_empty() {
            out.push(format!("  arguments: {}", p.args.join(", ")));
        }
        if !p.labels.is_empty() {
            out.push(format!("  labels: {}", p.labels.join(", ")));
        }
        for e in &p.expansions {
            if let Some(err) = &e.error {
                out.push(format!("  expanded, {}: {err}", e.case));
            }
            for s in &e.steps {
                let name = if p.command.is_some() {
                    String::new()
                } else {
                    format!(", step {}", s.name)
                };
                out.push(format!("  expanded, {}{name}: {}", e.case, s.command));
            }
        }
    }
    out
}

/// Whether `profile check` passes: there is at least one profile and the secrets are fine.
pub fn verdict(profiles: &[Profile], secrets_ok: bool) -> bool {
    !profiles.is_empty() && secrets_ok
}
