use crate::api::{JobDetail, PoolLoad, QueueEvent, RunnerInfo};
use crate::ipc::BrokerStatus;
use crate::stats_view::StatsView;
use lr_core::Timestamp;
use serde::{Deserialize, Serialize};

/// Where a job of the dashboard stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DashboardList {
    /// Executing on a runner.
    Running,
    /// Waiting in a broker queue.
    Queued,
    /// Waiting in the hub's outbox for an unreachable broker.
    Outbox,
    /// Finished.
    History,
    /// A list of a newer minor version.
    #[serde(other)]
    Unknown,
}

/// One job as every tab of the dashboard shows it: the full card plus its place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardJob {
    /// Broker that holds it; `None` for an order still in the outbox without a target.
    pub broker: Option<String>,
    /// Which list it belongs to.
    pub list: DashboardList,
    /// Position in the queue from 1.
    #[serde(default)]
    pub position: Option<u32>,
    /// The card: summary, project, source, label, note, branch, directory, steps.
    pub detail: JobDetail,
    /// Last lines of the current step of a running job.
    #[serde(default)]
    pub last_lines: Vec<String>,
    /// The broker is unreachable and this row is the last known value.
    #[serde(default)]
    pub stale: bool,
    /// The kind of the wait of a queued job, as [`crate::QueueItem::wait_code`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_code: Option<String>,
    /// The effective priority of a queued job, as [`crate::QueueItem::priority`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    /// What the job asked for, as [`crate::QueueItem::requested`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested: Option<crate::Requested>,
}

/// The runners of one broker with its pool load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerRunners {
    /// Broker name.
    pub broker: String,
    /// Pool load.
    pub pool: PoolLoad,
    /// Runners.
    pub runners: Vec<RunnerInfo>,
    /// The broker is unreachable and these are the last known values.
    #[serde(default)]
    pub stale: bool,
}

/// Everything the dashboard page shows at one moment (spec 9.1, 9.3); one model feeds the
/// live page, the HTML snapshot and the JSON export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardSnapshot {
    /// When the snapshot was taken.
    pub taken_at: Timestamp,
    /// Known brokers with availability and outbox size.
    pub brokers: Vec<BrokerStatus>,
    /// Running, queued, outbox and finished jobs.
    pub jobs: Vec<DashboardJob>,
    /// Runners per broker.
    pub runners: Vec<BrokerRunners>,
    /// Statistics.
    pub stats: StatsView,
    /// Which part of the history the finished jobs are; absent when the brokers do not report
    /// the total, so the page cannot page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_page: Option<HistoryPage>,
    /// How many queued orders the rows show against how many the brokers hold; absent when
    /// the brokers do not report the total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_count: Option<GridCount>,
}

/// The part of the history a snapshot carries: `shown` runs from run number `offset` (from 0,
/// newest first) of `total`, asked as pages of `limit`. With several brokers each one is paged
/// by the same offset and limit and the totals add up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPage {
    /// Runs skipped on each broker.
    pub offset: u64,
    /// Page size asked of each broker.
    pub limit: u64,
    /// Runs the brokers hold in all.
    pub total: u64,
}

/// A grid that a hard limit may have cut: the rows it shows and the rows there are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridCount {
    /// Rows shown.
    pub shown: u64,
    /// Rows the brokers hold.
    pub total: u64,
}

/// A live update of the dashboard: an event of one broker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardEvent {
    /// Broker that sent it.
    pub broker: String,
    /// The event.
    pub event: QueueEvent,
}
