//! Runner agent (spec 3.3, 3.9-3.11, 3.13): connects to its broker, runs one job at a time
//! through a [`lr_abstractions::ProcessRunner`], prepares the working directory (`source`
//! step), keeps named caches, and delivers output and results through its outbox with `seq`
//! per job and direction, `resume` and replay after reconnects and restarts.

mod agent;
mod background;
mod cache;
mod connect;
mod diag;
mod disk;
mod job;
mod metrics;
mod outbox;
mod output;
mod prepare;
mod run;
mod service_book;
mod service_config;
mod service_run;
mod service_stop;
mod services;
mod settings;
mod source;

pub use agent::{AgentControl, AgentExit, BrokerAddress, RunnerAgent, RunnerDeps};
pub use cache::{plan_sweep, CacheLayout, WORKSPACE_CLIENT};
pub use service_config::{
    Lifetime, Reuse, ServiceSpec, DEFAULT_READY_TIMEOUT_S, DEFAULT_RESTARTS, DEFAULT_STOP_TIMEOUT_S,
};
pub use settings::{
    Retention, RunnerSettings, SettingsError, DEFAULT_LOST_KILL_S, MAX_HEARTBEAT_S, MAX_LOST_KILL_S,
};
pub use source::{
    plan_source, quote_arg, validate_branch, validate_commit, validate_repo_url, SourceError,
    SourceInput, SourcePlan, WorkspaceLayout, SOURCE_STEP,
};
