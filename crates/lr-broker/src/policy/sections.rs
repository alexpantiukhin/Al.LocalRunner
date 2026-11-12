use lr_core::{GroupBy, Strategy, Zone, DEFAULT_AGE_AFTER_MIN};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use std::collections::BTreeSet;
use std::num::{NonZeroU32, NonZeroU64};
use std::str::FromStr;

fn nz(v: u64) -> NonZeroU64 {
    NonZeroU64::new(v).expect("non-zero default")
}

fn nz32(v: u32) -> NonZeroU32 {
    NonZeroU32::new(v).expect("non-zero default")
}

fn parsed<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr<Err = String>,
{
    String::deserialize(d)?.parse().map_err(D::Error::custom)
}

fn zone_order<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Zone>, D::Error> {
    let names = Vec::<String>::deserialize(d)?;
    let mut zones = Vec::with_capacity(names.len());
    for name in names {
        let zone: Zone = name.parse().map_err(D::Error::custom)?;
        if zones.contains(&zone) {
            return Err(D::Error::custom(format!("zone `{zone}` is listed twice")));
        }
        zones.push(zone);
    }
    Ok(zones)
}

/// `[queue]`: ordering strategy and aging.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct QueueSection {
    /// `strategy`, default `priority-fifo`.
    #[serde(deserialize_with = "parsed")]
    pub strategy: Strategy,
    /// `age_after_min`, default 30.
    pub age_after_min: u64,
}

impl Default for QueueSection {
    fn default() -> Self {
        QueueSection {
            strategy: Strategy::PriorityFifo,
            age_after_min: DEFAULT_AGE_AFTER_MIN,
        }
    }
}

/// `[clients]`: what forms a group.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ClientsSection {
    /// `group_by`, default `label`.
    #[serde(deserialize_with = "parsed")]
    pub group_by: GroupBy,
}

/// `[join]`: join codes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct JoinSection {
    /// `code_ttl_min`, default 60 (spec 3.8: one hour).
    pub code_ttl_min: NonZeroU64,
}

impl Default for JoinSection {
    fn default() -> Self {
        JoinSection {
            code_ttl_min: nz(60),
        }
    }
}

/// `[orphans] queued`: what happens to waiting orders of a long-absent hub.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrphanQueued {
    /// Cancel them.
    #[default]
    Cancel,
    /// Keep them queued.
    Keep,
}

/// `[orphans] running`: what happens to running jobs of a long-absent hub.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrphanRunning {
    /// Let them finish; results wait in the outbox.
    #[default]
    Finish,
    /// Cancel them.
    Cancel,
}

/// `[orphans]`: orders of a hub that has not been seen for long.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OrphansSection {
    /// `absent_after_min`, default 360.
    pub absent_after_min: NonZeroU64,
    /// `queued`, default `cancel`.
    pub queued: OrphanQueued,
    /// `running`, default `finish`.
    pub running: OrphanRunning,
}

impl Default for OrphansSection {
    fn default() -> Self {
        OrphansSection {
            absent_after_min: nz(360),
            queued: OrphanQueued::Cancel,
            running: OrphanRunning::Finish,
        }
    }
}

/// `[grace]`: the grace period of runner `lost` and of the runner killing jobs while the
/// broker is unreachable.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GraceSection {
    /// `lost_min`, default 3.
    pub lost_min: NonZeroU64,
}

impl Default for GraceSection {
    fn default() -> Self {
        GraceSection { lost_min: nz(3) }
    }
}

/// `[results]`: results kept for an absent hub.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ResultsSection {
    /// `keep_hours`, default 72.
    pub keep_hours: NonZeroU64,
}

impl Default for ResultsSection {
    fn default() -> Self {
        ResultsSection { keep_hours: nz(72) }
    }
}

/// `[cancel] others`: who may cancel orders of other clients.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum CancelOthers {
    /// `none`: nobody.
    #[serde(rename = "none")]
    Nobody,
    /// `owner`: only the broker owner over the local channel (default, spec 7.1).
    #[default]
    #[serde(rename = "owner")]
    Owner,
}

/// `[cancel]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CancelSection {
    /// `others`, default `owner`.
    pub others: CancelOthers,
}

/// One `[[secrets.allow]]` or `[[services.allow]]` entry: a label and the names it may ask for.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowEntry {
    /// Client label.
    pub label: String,
    /// Allowed names.
    pub names: Vec<String>,
}

/// `[secrets]`: only `[[secrets.allow]]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SecretsSection {
    /// Entries in file order; the first entry of a label decides. Empty: no secrets at all.
    pub allow: Vec<AllowEntry>,
}

/// `[pool]`: the pool budget.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PoolSection {
    /// `mem_mb`; `None` means all memory of the broker host.
    pub mem_mb: Option<NonZeroU64>,
    /// `cpu`; `None` means all cores of the broker host.
    pub cpu: Option<NonZeroU32>,
    /// `exclusive_tags`, default none.
    pub exclusive_tags: BTreeSet<String>,
}

/// `[limits]`: ceilings of the broker.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsSection {
    /// `max_job_timeout_s`: intake ceiling of an order timeout; default none.
    pub max_job_timeout_s: Option<NonZeroU64>,
    /// `timeout_above_max`: what intake does with a timeout above `max_job_timeout_s`.
    pub timeout_above_max: TimeoutAboveMax,
    /// `job_log_max_mb`: log of one job, default 256 (spec 8).
    pub job_log_max_mb: NonZeroU64,
    /// `outbox_max_mb`: broker outbox per recipient, default 512 (spec 8).
    pub outbox_max_mb: NonZeroU64,
    /// `max_active_per_client`: active orders per client key, default 100 (spec 8).
    pub max_active_per_client: NonZeroU32,
    /// `api_requests_per_min`: orders and read requests per client key in any 60 seconds,
    /// default 1200; above it the request is refused `rejected_rate_limited` (spec 8).
    pub api_requests_per_min: NonZeroU32,
}

impl Default for LimitsSection {
    fn default() -> Self {
        LimitsSection {
            max_job_timeout_s: None,
            timeout_above_max: TimeoutAboveMax::Reject,
            job_log_max_mb: nz(256),
            outbox_max_mb: nz(512),
            max_active_per_client: nz32(100),
            api_requests_per_min: nz32(1200),
        }
    }
}

/// `[limits] timeout_above_max` (spec 3.10).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutAboveMax {
    /// The order is rejected `rejected_timeout` (default).
    #[default]
    Reject,
    /// The order is accepted with the timeout trimmed to the maximum, and told so.
    Trim,
}

/// `[routing]`: preference between suitable runners.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RoutingSection {
    /// `runner_zone_order`, default local, lan, remote.
    #[serde(deserialize_with = "zone_order")]
    pub runner_zone_order: Vec<Zone>,
}

impl Default for RoutingSection {
    fn default() -> Self {
        RoutingSection {
            runner_zone_order: vec![Zone::Local, Zone::Lan, Zone::Remote],
        }
    }
}

/// `[history]`: retention of the broker history (spec 8).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HistorySection {
    /// `keep_days`: age of kept logs, default 30.
    pub keep_days: NonZeroU64,
    /// `logs_max_gb`: total size of kept logs, default 2.
    pub logs_max_gb: NonZeroU64,
    /// `rows_keep_days`: age of kept history rows, default 365.
    pub rows_keep_days: NonZeroU64,
}

impl Default for HistorySection {
    fn default() -> Self {
        HistorySection {
            keep_days: nz(30),
            logs_max_gb: nz(2),
            rows_keep_days: nz(365),
        }
    }
}

/// A percentage from 1 to 100.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "u64")]
pub struct Percent(u8);

impl Percent {
    /// The value.
    pub fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u64> for Percent {
    type Error = String;

    fn try_from(v: u64) -> Result<Self, String> {
        match u8::try_from(v) {
            Ok(p @ 1..=100) => Ok(Percent(p)),
            _ => Err(format!("{v} is not a percentage from 1 to 100")),
        }
    }
}

/// `[saturation]`: thresholds for statistics and the dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SaturationSection {
    /// `queue_wait_min`: "the queue stalls" threshold, default 15.
    pub queue_wait_min: NonZeroU64,
    /// `mem_percent`: pool memory load threshold, default 90.
    pub mem_percent: Percent,
}

impl Default for SaturationSection {
    fn default() -> Self {
        SaturationSection {
            queue_wait_min: nz(15),
            mem_percent: Percent(90),
        }
    }
}

/// `[cache] share`: who sees a shared runner cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheShare {
    /// Per client key (default, spec 3.10).
    #[default]
    Client,
    /// Shared by the clients of a project.
    Project,
}

/// `[cache]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CacheSection {
    /// `share`, default `client`.
    pub share: CacheShare,
}

/// `[services]` and `[[services.allow]]` (spec 3.11).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServicesSection {
    /// `evict_idle`, default true.
    pub evict_idle: bool,
    /// Entries in file order; the first entry of a label decides. Empty: any label, any service.
    pub allow: Vec<AllowEntry>,
}

impl Default for ServicesSection {
    fn default() -> Self {
        ServicesSection {
            evict_idle: true,
            allow: Vec::new(),
        }
    }
}

/// `[visibility] others_orders` (spec 3.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum OthersOrders {
    /// `none`: only pool load.
    #[serde(rename = "none")]
    Hidden,
    /// `summary`: identifier, state, platform, runner, times and client label (default).
    #[default]
    #[serde(rename = "summary")]
    Summary,
    /// `full`: every order field, never the log.
    #[serde(rename = "full")]
    Full,
}

/// `[visibility]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct VisibilitySection {
    /// `others_orders`, default `summary`.
    pub others_orders: OthersOrders,
}

/// Which commands an order may carry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandMode {
    /// Any command (default: a client with a key is trusted).
    #[default]
    Any,
    /// Only names from the runner's list.
    Named,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommandClient {
    label: Option<String>,
    key: Option<String>,
    mode: CommandMode,
}

/// One `[[commands.client]]`: a mode for a label, a client key, or both.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawCommandClient")]
pub struct CommandClient {
    /// Client label it applies to.
    pub label: Option<String>,
    /// Client key it applies to.
    pub key: Option<String>,
    /// Mode for matching clients.
    pub mode: CommandMode,
}

impl TryFrom<RawCommandClient> for CommandClient {
    type Error = String;

    fn try_from(raw: RawCommandClient) -> Result<Self, String> {
        if raw.label.is_none() && raw.key.is_none() {
            return Err("a [[commands.client]] entry needs `label` or `key`".into());
        }
        Ok(CommandClient {
            label: raw.label,
            key: raw.key,
            mode: raw.mode,
        })
    }
}

impl CommandClient {
    /// Whether the entry applies to a client with `key` and `label`.
    pub fn matches(&self, key: &str, label: &str) -> bool {
        self.key.as_deref().is_none_or(|k| k == key)
            && self.label.as_deref().is_none_or(|l| l == label)
    }
}

/// `[commands]` (spec 3.5).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CommandsSection {
    /// `default`, default `any`.
    pub default: CommandMode,
    /// Entries in file order; the first matching one decides.
    pub client: Vec<CommandClient>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawLocalUser {
    name: NonEmpty,
    #[serde(default)]
    label: String,
}

#[derive(Deserialize)]
#[serde(try_from = "String")]
struct NonEmpty(String);

impl TryFrom<String> for NonEmpty {
    type Error = &'static str;

    fn try_from(s: String) -> Result<Self, &'static str> {
        if s.trim().is_empty() {
            Err("must not be empty")
        } else {
            Ok(NonEmpty(s))
        }
    }
}

/// One `[[local_user]]` (spec 3.8): an OS user allowed on local IPC and its client label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalUserEntry {
    /// OS account name.
    pub name: String,
    /// Label of its client record; default empty.
    pub label: String,
}

impl From<RawLocalUser> for LocalUserEntry {
    fn from(raw: RawLocalUser) -> Self {
        LocalUserEntry {
            name: raw.name.0,
            label: raw.label,
        }
    }
}
