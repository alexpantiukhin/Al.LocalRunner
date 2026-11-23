use super::target::{resolve, Target};
use crate::host;
use crate::paths::Layout;
use crate::runner_config::RunnerConfig;
use std::path::PathBuf;

fn runner_warnings(name: &str, text: &str) -> Result<Vec<String>, String> {
    RunnerConfig::parse(
        text,
        Some(name),
        name,
        PathBuf::from("."),
        host::resources(),
    )
    .map(|c| c.warnings)
    .map_err(|e| e.to_string())
}

/// Checks the edited `text` of `target` the way the role loads it and returns warnings; an
/// error means the file must not be written. A runner file may keep the unknown keys it had,
/// but a change must not add one.
pub fn validate(target: &Target, text: &str, layout: &Layout) -> Result<Vec<String>, String> {
    match target {
        Target::Broker => {
            let policy = lr_broker::policy::load(text).map_err(|e| e.to_string())?;
            Ok(lr_broker::policy::check(&policy, &host::resources())
                .iter()
                .map(ToString::to_string)
                .collect())
        }
        Target::Hub => lr_hub::policy::parse_hub_policy(text)
            .map(|_| Vec::new())
            .map_err(|e| e.to_string()),
        Target::Logging => lr_obs::LoggingConfig::parse(text)
            .map(|_| Vec::new())
            .map_err(|e| e.to_string()),
        Target::Runner(name) => {
            let warnings = runner_warnings(name, text)?;
            let before = resolve(layout, target)
                .ok()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|old| runner_warnings(name, &old).ok())
                .unwrap_or_default();
            let mut old = before;
            for w in &warnings {
                match old.iter().position(|o| o == w) {
                    Some(i) => {
                        old.swap_remove(i);
                    }
                    None => return Err(w.clone()),
                }
            }
            Ok(warnings)
        }
    }
}
