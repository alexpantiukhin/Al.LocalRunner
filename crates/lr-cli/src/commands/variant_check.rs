//! `profile check`: what is wrong with the variants of a profile before any run.

use crate::paths::Layout;
use lr_core::{Profile, Zone};
use std::collections::BTreeSet;

/// What a runner of this installation declares, as far as variant filters look at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerFacts {
    /// The runner name.
    pub name: String,
    /// Its platforms.
    pub platforms: BTreeSet<String>,
    /// Its tags.
    pub tags: BTreeSet<String>,
    /// The programs its `runner.toml` declares.
    pub programs: BTreeSet<String>,
}

/// The runners of this installation with what they declare; one whose file does not read is
/// left out.
pub fn local_facts(layout: &Layout) -> Vec<RunnerFacts> {
    let host = crate::host::resources();
    layout
        .runner_names()
        .into_iter()
        .filter_map(|name| {
            let path = crate::build::runner_config_path(layout, &name, None)?;
            let text = std::fs::read_to_string(path).ok()?;
            let cfg = crate::runner_config::RunnerConfig::parse(
                &text,
                Some(name.as_str()),
                &crate::facts::host_name(),
                layout.root.clone(),
                host,
            )
            .ok()?;
            Some(RunnerFacts {
                name,
                platforms: cfg.settings.platforms.clone(),
                tags: cfg.settings.tags.clone(),
                programs: cfg.programs.keys().cloned().collect(),
            })
        })
        .collect()
}

/// The warnings about the variants of `profile`: a variant that an earlier one always hides,
/// variants that can match the same runner (the first wins), and a variant that no runner of
/// this installation matches (known only when this installation has runners).
pub fn variant_warnings(profile: &Profile, runners: &[RunnerFacts]) -> Vec<String> {
    let mut out = Vec::new();
    let variants = &profile.variants;
    for (j, later) in variants.iter().enumerate() {
        for earlier in &variants[..j] {
            if later.when.is_within(&earlier.when) {
                out.push(format!(
                    "profile {}: variant {} is never used: variant {}, tried first, matches every runner it matches",
                    profile.name, later.name, earlier.name
                ));
            } else if later.when.overlaps(&earlier.when) {
                out.push(format!(
                    "profile {}: variants {} and {} can match the same runner; the first one wins",
                    profile.name, earlier.name, later.name
                ));
            }
        }
    }
    if runners.is_empty() {
        return out;
    }
    for v in variants {
        let reachable_here = v.when.zones.is_empty()
            || v.when.zones.iter().any(|z| matches!(z, Zone::Local | Zone::Shared));
        if !reachable_here {
            continue;
        }
        let any = runners.iter().any(|r| {
            v.when
                .matches(Zone::Local, &r.platforms, &r.tags, &r.programs, &r.name)
        });
        if !any {
            out.push(format!(
                "profile {}: variant {}: no runner of this installation matches ({}); it helps only when a joined runner of another machine does",
                profile.name, v.name, v.when
            ));
        }
    }
    out
}
