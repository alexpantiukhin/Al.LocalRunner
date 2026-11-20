use serde::{Deserialize, Serialize};

/// Median and 95th percentile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpreadView {
    /// 50th percentile.
    pub median: Option<u64>,
    /// 95th percentile.
    pub p95: Option<u64>,
}

/// Statistics of one step name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepStatsView {
    /// Step name.
    pub name: String,
    /// Steps that started.
    pub runs: u64,
    /// Steps that passed.
    pub passed: u64,
    /// Duration in ms.
    pub duration_ms: SpreadView,
}

/// Declared and real memory of a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryStatsView {
    /// `mem_mb` of the latest run.
    pub declared_mb: Option<u64>,
    /// Runs that reported a peak.
    pub measured: u64,
    /// Median peak, MB.
    pub median_mb: Option<u64>,
    /// Greatest peak, MB.
    pub max_mb: Option<u64>,
}

/// Statistics of one profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileStatsView {
    /// Profile name.
    pub profile: String,
    /// Final runs.
    pub runs: u64,
    /// Passed.
    pub passed: u64,
    /// Failed.
    pub failed: u64,
    /// Timed out.
    pub timeouts: u64,
    /// Cancelled.
    pub cancelled: u64,
    /// Lost.
    pub lost: u64,
    /// Wait in ms.
    pub wait_ms: SpreadView,
    /// Duration in ms.
    pub duration_ms: SpreadView,
    /// Per step.
    pub steps: Vec<StepStatsView>,
    /// Declared and real memory; absent from an older broker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem: Option<MemoryStatsView>,
}

/// How much of the pool a client used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientUsageView {
    /// Client key.
    pub client: String,
    /// Client label.
    pub label: String,
    /// Final runs.
    pub runs: u64,
    /// Sum of run durations in ms.
    pub busy_ms: u64,
}

/// The `stats` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatsView {
    /// Per profile.
    pub profiles: Vec<ProfileStatsView>,
    /// Per client.
    pub clients: Vec<ClientUsageView>,
}
