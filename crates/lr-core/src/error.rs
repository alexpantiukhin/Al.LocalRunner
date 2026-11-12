use thiserror::Error;

/// Failure while reading or resolving profiles.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProfileError {
    /// The file is not valid TOML.
    #[error("profiles.toml: {0}")]
    Syntax(String),
    /// A profile field is missing or has a wrong value.
    #[error("profile `{profile}`, field `{field}`: {reason}")]
    Invalid {
        /// Profile name.
        profile: String,
        /// Field path inside the profile.
        field: String,
        /// What is wrong.
        reason: String,
    },
    /// The profile does not support the requested platform.
    #[error("profile `{profile}` does not support platform `{platform}`")]
    UnsupportedPlatform {
        /// Profile name.
        profile: String,
        /// Requested platform.
        platform: String,
    },
}

impl ProfileError {
    pub(crate) fn invalid(profile: &str, field: &str, reason: impl Into<String>) -> Self {
        ProfileError::Invalid {
            profile: profile.to_string(),
            field: field.to_string(),
            reason: reason.into(),
        }
    }
}
