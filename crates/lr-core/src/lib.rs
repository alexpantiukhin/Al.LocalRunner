//! Generic core model: jobs, profiles, requirements and runner capabilities.

pub mod admission;
mod condition;
pub mod error;
pub mod identity;
pub mod matching;
pub mod model;
pub mod policy;
pub mod predict;
pub mod profile;
mod profile_args;
mod profile_bind;
mod profile_parse;
pub mod queue;
pub mod requirements;
pub mod routing;
pub mod scheduler;
pub mod source_github;
pub mod strategy;
pub mod zone;

pub use admission::{
    same_host, Admission, ForeignLoadMode, LoadResource, MachineLoad, DEFAULT_MEM_HEADROOM_MB,
};
pub use condition::{glob_match, Condition, NumAttr, NumTest, StrAttr, StrTest, Term};
pub use error::ProfileError;
pub use identity::{
    identity_from_session, may_cancel, normalize_origin, project_name, validate_label,
    validate_note, GithubInfo, Identity, IdentityError, SenderFacts, Source,
};
pub use matching::{satisfies, Accept, Capabilities, Mismatch, MismatchReason, ServiceStatus};
pub use model::{JobId, JobState, StepState, Timestamp, Zone, MAX_FRAME_BYTES};
pub use policy::{GroupBy, Policy, Quota, Reserve, Rule, RulePolicy, Strategy};
pub use predict::{earliest_start, pick_next, Plan, QueueState};
pub use profile::{resolve_for_platform, PlatformOverride, Profile, Program, StepSpec};
pub use profile_args::{ArgSpec, RESERVED_ARG_NAMES};
pub use profile_bind::bind_args;
pub use profile_parse::parse_profiles;
pub use queue::{
    Assignment, ClientView, Declared, GroupUsage, Order, QueueEntry, Reservation, RunnerView,
    RunningJob, ServeLog,
};
pub use requirements::{cache_env_var, MachineRef, Requirements};
pub use routing::{check_runners, sort_by_preference, suitable};
pub use scheduler::{Free, PoolLimits, Reject, Scheduler, Wait};
pub use source_github::identity_from_github;
pub use strategy::dispatch_order;
pub use zone::{network_of, zone_between, zone_of, Endpoint, NamedNetworks, Site};
