//! Generic core model: jobs, profiles, requirements and runner capabilities.

pub mod error;
pub mod matching;
pub mod model;
pub mod profile;
mod profile_parse;
pub mod requirements;

pub use error::ProfileError;
pub use matching::{satisfies, Accept, Capabilities, Mismatch, MismatchReason, ServiceStatus};
pub use model::{JobId, JobState, StepState, Zone};
pub use profile::{resolve_for_platform, PlatformOverride, Profile, Program, StepSpec};
pub use profile_parse::parse_profiles;
pub use requirements::{cache_env_var, MachineRef, Requirements};
