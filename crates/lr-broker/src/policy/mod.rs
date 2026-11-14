//! Loader of the broker's `policy.toml` (spec 3.5): the file format, its defaults and checks,
//! `policy reload` semantics and the conversion to the core [`Policy`].

mod admission;
mod error;
mod explain;
mod file;
mod listen;
mod reload;
mod resources;
mod rules;
mod sections;
mod validate;
mod when;

pub use admission::{AdmissionSection, ForeignLoad};
pub use error::{PolicyError, PolicyWarning};
pub use explain::explain_rules;
pub use file::{load, HostResources, PolicyFile};
pub use listen::{ListenSection, ListenValue};
pub use reload::{ActivePolicy, Applied, RestartOnly};
pub use resources::{Headroom, ResourceMode, ResourcesSection, DEFAULT_HEADROOM};
pub use rules::{default_rules, RuleEntry, DEFAULT_CI_LABEL, DEFAULT_CI_PRIORITY, DEFAULT_CI_RULE};
pub use sections::{
    AllowEntry, CacheSection, CacheShare, CancelOthers, CancelSection, ClientsSection,
    CommandClient, CommandMode, CommandsSection, GraceSection, HistorySection, JoinSection,
    LimitsSection, LocalUserEntry, OrphanQueued, OrphanRunning, OrphansSection, OthersOrders,
    Percent, PoolSection, QueueSection, ResultsSection, RoutingSection, SaturationSection,
    SecretsSection, ServicesSection, TimeoutAboveMax, VisibilitySection,
};
pub use validate::{check, WIDE_V4_PREFIX, WIDE_V6_PREFIX};

use lr_core::{Policy, RulePolicy};

/// The core rule policy of `file`; `host` fills the parts of `[pool]` the owner leaves out.
pub fn rule_policy(file: &PolicyFile, host: &HostResources) -> RulePolicy {
    RulePolicy {
        strategy: file.queue.strategy,
        rules: file.rules.iter().map(|e| e.rule.clone()).collect(),
        group_by: file.clients.group_by,
        pool: file.pool.limits(host),
        max_job_timeout_s: file.limits.max_job_timeout_s.map(|v| v.get()),
        runner_zone_order: file.routing.runner_zone_order.clone(),
        admission: file.admission.admission(),
    }
}

/// The policy the broker's planner asks, built from `file` (see [`rule_policy`]).
pub fn into_policy(file: &PolicyFile, host: &HostResources) -> Box<dyn Policy + Send + Sync> {
    Box::new(rule_policy(file, host))
}
