//! The runner's [`SecretSource`] from the `[secret.*]` sources of `runner.toml` (spec 3.13).
//! The assembly wraps it in [`lr_obs::RedactingSecrets`], so every value it hands out is also
//! masked in the diagnostics.

use crate::runner_config::SecretFrom;
use async_trait::async_trait;
use lr_abstractions::{SecretError, SecretSource, SecretValue};
use std::collections::BTreeMap;

/// Secrets resolved from the environment of the runner process or from a file; the OS keychain
/// source is recognised but not read by this build, so such a secret is reported unavailable.
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
            Some(SecretFrom::File(path)) => read_file(name, path.clone()).await,
            Some(SecretFrom::Keychain(_)) => Err(SecretError::Unavailable {
                name: name.to_string(),
                reason: "the OS keychain source is not supported by this build".into(),
            }),
        }
    }
}

async fn read_file(name: &str, path: std::path::PathBuf) -> Result<SecretValue, SecretError> {
    let read = tokio::task::spawn_blocking(move || std::fs::read(path)).await;
    let unavailable = |reason: String| SecretError::Unavailable {
        name: name.to_string(),
        reason,
    };
    let bytes = match read {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(SecretError::NotFound(name.to_string()))
        }
        Ok(Err(e)) => return Err(unavailable(format!("the file cannot be read: {e}"))),
        Err(_) => return Err(unavailable("the read of the file did not finish".into())),
    };
    let text =
        String::from_utf8(bytes).map_err(|_| unavailable("the file is not UTF-8 text".into()))?;
    let value = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(&text);
    match value.is_empty() {
        true => Err(SecretError::NotFound(name.to_string())),
        false => Ok(SecretValue::new(value)),
    }
}
