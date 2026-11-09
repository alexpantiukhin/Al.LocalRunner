use async_trait::async_trait;
use std::fmt;

/// A secret value. It has no `Display` and no serialization, and its `Debug` hides the value,
/// so it cannot reach a log, a metric, an order or the history by accident (spec 3.13).
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    /// Wraps a value.
    pub fn new(value: impl Into<String>) -> SecretValue {
        SecretValue(value.into())
    }

    /// The value itself, for the job environment and for masking only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue(***)")
    }
}

/// Why a secret could not be read; the job then ends with `secret_missing` instead of running
/// without it (spec 3.13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretError {
    /// No secret of this name is configured, or its source (variable, entry) does not exist.
    NotFound(String),
    /// The source exists but cannot be read now (locked keychain and the like).
    Unavailable {
        /// Secret name.
        name: String,
        /// What went wrong, without the value.
        reason: String,
    },
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecretError::NotFound(name) => write!(f, "secret not found: {name}"),
            SecretError::Unavailable { name, reason } => {
                write!(f, "secret {name} unavailable: {reason}")
            }
        }
    }
}

impl std::error::Error for SecretError {}

/// The runner's secret store: resolves a secret name through the `[secret.*]` sources of
/// `runner.toml` (environment of the runner process, OS keychain). Secrets live only on the
/// runner; the broker only decides which names a client may request.
#[async_trait]
pub trait SecretSource: Send + Sync {
    /// The value of the secret `name`.
    async fn get(&self, name: &str) -> Result<SecretValue, SecretError>;
}
