//! Generic core model: jobs, profiles, requirements and runner capabilities.

pub mod error;
pub mod identity;
pub mod matching;
pub mod model;
pub mod profile;
mod profile_parse;
pub mod requirements;
pub mod source_github;
pub mod zone;

pub use error::ProfileError;
pub use identity::{
    identity_from_session, may_cancel, normalize_origin, project_name, validate_label,
    validate_note, GithubInfo, Identity, IdentityError, SenderFacts, Source,
};
pub use matching::{satisfies, Accept, Capabilities, Mismatch, MismatchReason, ServiceStatus};
pub use model::{JobId, JobState, StepState, Zone};
pub use profile::{resolve_for_platform, PlatformOverride, Profile, Program, StepSpec};
pub use profile_parse::parse_profiles;
pub use requirements::{cache_env_var, MachineRef, Requirements};
pub use source_github::identity_from_github;
pub use zone::zone_of;
