//! `profile check`: the per-profile summary and its rendering.

use lr_core::{Profile, StepSpec};
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
    /// `"by commit"` when the profile has platforms or platform overrides, else `"by work tree"`.
    pub platform_mode: &'static str,
    /// The platforms and the override keys, sorted.
    pub platforms: Vec<String>,
    /// Names of the arguments.
    pub args: Vec<String>,
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
            "by work tree"
        } else {
            "by commit"
        },
        platforms,
        args: p.args.iter().map(|a| a.name.clone()).collect(),
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
        let parts: Vec<String> = [
            p.mem_mb.map(|v| format!("mem {v} MB")),
            p.cpu.map(|v| format!("cpu {v}")),
            p.disk_mb.map(|v| format!("disk {v} MB")),
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
    }
    out
}

/// Whether `profile check` passes: there is at least one profile and the secrets are fine.
pub fn verdict(profiles: &[Profile], secrets_ok: bool) -> bool {
    !profiles.is_empty() && secrets_ok
}
