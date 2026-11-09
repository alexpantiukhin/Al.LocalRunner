use crate::identity::{Identity, Source};
use crate::matching::Capabilities;
use crate::model::{JobId, Timestamp, Zone};
use crate::requirements::Requirements;
use crate::zone::Endpoint;
use std::collections::BTreeMap;

/// Attributes the order declares about itself; the client may name them as it likes, so they
/// never raise priority on their own (spec 3.5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Declared {
    /// `session` or `github`.
    pub source: Option<String>,
    /// Project name.
    pub project: Option<String>,
    /// Label `--as` of the submitter.
    pub label: Option<String>,
}

impl Declared {
    /// Declared attributes of a submitter identity.
    pub fn from_identity(id: &Identity) -> Declared {
        Declared {
            source: Some(
                match id.source {
                    Source::Session => "session",
                    Source::Github(_) => "github",
                }
                .to_string(),
            ),
            project: Some(id.project.clone()),
            label: Some(id.label.clone()),
        }
    }
}

/// The scheduling view of an order (spec 3.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    /// Job identifier from the hub.
    pub job: JobId,
    /// What the job needs from a runner.
    pub requirements: Requirements,
    /// When the hub enqueued the job; carried in the order and kept on re-brokering, so aging
    /// and FIFO are measured from it (spec 6).
    pub enqueued_at: Timestamp,
    /// Declared attributes.
    pub declared: Declared,
    /// Priority the order wishes; only a wish, the policy decides.
    pub wanted_priority: Option<i64>,
}

/// Trusted attributes of the client, from its key record, and its connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientView {
    /// Client key identifier.
    pub key: String,
    /// Client label set by the broker owner.
    pub label: String,
    /// MachineId of the client from the key record.
    pub machine_id: String,
    /// How the client is connected.
    pub endpoint: Endpoint,
}

/// A queued order with its client, as the broker holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueEntry {
    /// The order.
    pub order: Order,
    /// Its client.
    pub client: ClientView,
    /// When this broker accepted it; informational, aging does not use it.
    pub accepted_at: Timestamp,
}

/// A runner registered at the broker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerView {
    /// Runner name, unique on the broker.
    pub name: String,
    /// What the runner offers; `zone` is recomputed per client by the planner.
    pub caps: Capabilities,
    /// How the runner is connected.
    pub endpoint: Endpoint,
    /// Whether the runner is online.
    pub online: bool,
}

/// A job that runs now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningJob {
    /// The order and its client.
    pub entry: QueueEntry,
    /// Runner that executes it.
    pub runner: String,
    /// Start on the broker clock.
    pub started_at: Timestamp,
}

impl RunningJob {
    /// The latest moment it frees its runner and resources: start plus `timeout_s`.
    pub fn deadline(&self) -> Timestamp {
        self.started_at
            .plus_secs(self.entry.order.requirements.timeout_s)
    }
}

/// What a group has received so far, for `fair-share` and `weighted`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GroupUsage {
    /// Last time the group got a runner.
    pub last_granted: Option<Timestamp>,
    /// How many jobs of the group were started.
    pub granted: u64,
}

/// Usage per group, kept by the broker.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServeLog {
    /// Usage by group key.
    pub groups: BTreeMap<String, GroupUsage>,
}

impl ServeLog {
    /// Records that a job of `group` was started at `at`.
    pub fn record(&mut self, group: &str, at: Timestamp) {
        let usage = self.groups.entry(group.to_string()).or_default();
        usage.last_granted = Some(at);
        usage.granted += 1;
    }

    /// Usage of `group`, zero when it never got anything.
    pub fn usage(&self, group: &str) -> GroupUsage {
        self.groups.get(group).copied().unwrap_or_default()
    }
}

/// A decision of the planner: start `job` on `runner`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// The job.
    pub job: JobId,
    /// The runner.
    pub runner: String,
    /// Zone of the runner relative to the client.
    pub zone: Zone,
}

/// The earliest start reserved for the head order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    /// The head order.
    pub job: JobId,
    /// Its earliest possible start.
    pub at: Timestamp,
}
