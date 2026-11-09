use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Largest protocol frame, both ways: 4 MiB (spec 3.9). Shared by the protocol crate and the
/// transport contract so both refuse the same frames.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Identifier of a job, issued by the hub.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(pub String);

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Wall-clock time in milliseconds since the unix epoch.
///
/// Scheduling times are wall-clock values rather than `Instant`, because the hub's enqueue time
/// travels inside the order to another machine and must survive re-brokering (spec 6).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Timestamp(pub u64);

impl Timestamp {
    /// This time plus `secs` seconds, saturating.
    pub fn plus_secs(self, secs: u64) -> Timestamp {
        Timestamp(self.0.saturating_add(secs.saturating_mul(1000)))
    }

    /// Milliseconds from `earlier` to `self`, zero when `earlier` is later.
    pub fn millis_since(self, earlier: Timestamp) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}

impl fmt::Display for Timestamp {
    /// Formats as UTC `YYYY-MM-DD HH:MM:SS UTC`, dropping milliseconds.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secs = self.0 / 1000;
        let days = (secs / 86_400) as i64;
        let rem = secs % 86_400;
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        write!(
            f,
            "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
            rem / 3600,
            rem % 3600 / 60,
            rem % 60
        )
    }
}

/// Distance from the sender to a runner, ordered from nearest to farthest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Zone {
    /// Client and runner share the local IPC of one OS user.
    Local,
    /// Same network from the broker's `[networks]`.
    Lan,
    /// Everything else.
    Remote,
}

impl Zone {
    /// Lowercase name as used in configuration.
    pub fn as_str(self) -> &'static str {
        match self {
            Zone::Local => "local",
            Zone::Lan => "lan",
            Zone::Remote => "remote",
        }
    }
}

impl fmt::Display for Zone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Zone {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "local" => Ok(Zone::Local),
            "lan" => Ok(Zone::Lan),
            "remote" => Ok(Zone::Remote),
            other => Err(format!(
                "unknown zone `{other}`, expected local, lan or remote"
            )),
        }
    }
}

/// State of a job (spec 15.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    /// Waiting in the queue.
    Queued,
    /// Executing on a runner.
    Running,
    /// A cancel was accepted and the process tree is being stopped.
    Cancelling,
    /// The runner reported exit code 0.
    Passed,
    /// The runner reported a non-zero exit code.
    Failed,
    /// The job timeout expired.
    Timeout,
    /// The process tree is dead after a cancel.
    Cancelled,
    /// No news from the runner.
    Lost,
}

impl JobState {
    /// Whether the state is final and can never be overwritten.
    pub fn is_final(self) -> bool {
        matches!(
            self,
            JobState::Passed
                | JobState::Failed
                | JobState::Timeout
                | JobState::Cancelled
                | JobState::Lost
        )
    }

    /// Whether the transition `self -> next` is allowed.
    ///
    /// A waiting job is cancelled at once; a running job moves to `Cancelling`
    /// on an accepted cancel and its final state comes from the runner report.
    pub fn can_become(self, next: JobState) -> bool {
        use JobState::*;
        match self {
            Queued => matches!(next, Running | Cancelled),
            Running => matches!(next, Cancelling | Passed | Failed | Timeout | Lost),
            Cancelling => next.is_final(),
            _ => false,
        }
    }
}

/// State of a step (spec 15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepState {
    /// Not started yet.
    Queued,
    /// Executing.
    Running,
    /// Exited with code 0.
    Passed,
    /// Exited with a non-zero code.
    Failed,
    /// Killed on its timeout.
    Timeout,
    /// Not run because an earlier step failed.
    Skipped,
    /// Stopped by a cancel.
    Cancelled,
}
