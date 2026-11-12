//! The runner's [`SecretSource`] from the `[secret.*]` sources of `runner.toml` (spec 3.13).
//! The assembly wraps it in [`lr_obs::RedactingSecrets`], so every value it hands out is also
//! masked in the diagnostics.

use crate::runner_config::SecretFrom;
use async_trait::async_trait;
use lr_abstractions::{SecretError, SecretSource, SecretValue};
use std::collections::BTreeMap;

/// Secrets resolved from the environment of the runner process; the OS keychain source is
/// recognised but not read by this build, so such a secret is reported unavailable.
#[derive(Debug, Clone, Default)]
pub struct ConfigSecrets {
    sources: BTreeMap<String, SecretFrom>,
}

impl ConfigSecrets {
    /// The secrets of `runner.toml`.
    pub fn new(sources: BTreeMap<String, SecretFrom>) -> ConfigSecrets {
        ConfigSecrets { sources }
    }
}

#[async_trait]
impl SecretSource for ConfigSecrets {
    async fn get(&self, name: &str) -> Result<SecretValue, SecretError> {
        match self.sources.get(name) {
            None => Err(SecretError::NotFound(name.to_string())),
            Some(SecretFrom::Env(var)) => std::env::var(var)
                .ok()
                .filter(|v| !v.is_empty())
                .map(SecretValue::new)
                .ok_or_else(|| SecretError::NotFound(name.to_string())),
            Some(SecretFrom::Keychain(_)) => Err(SecretError::Unavailable {
                name: name.to_string(),
                reason: "the OS keychain source is not supported by this build".into(),
            }),
        }
    }
}
