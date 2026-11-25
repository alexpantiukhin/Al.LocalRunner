//! Arguments of the read commands (spec 8, 9.1, 11.1): `explain`, `history`, `log`, `stats`,
//! `runners` and the HTML snapshot of `status`.

use super::timefmt::{parse_since, Since};
use super::SourceArg;
use clap::{Args, Subcommand, ValueEnum};
use lr_core::JobState;
use std::path::PathBuf;

/// `--result` of `history`.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultArg {
    /// Waiting in the queue.
    Queued,
    /// Executing.
    Running,
    /// Being cancelled.
    Cancelling,
    /// Exit code 0.
    Passed,
    /// Non-zero exit code.
    Failed,
    /// The timeout expired.
    Timeout,
    /// Cancelled.
    Cancelled,
    /// Refused at intake and never run.
    Rejected,
    /// No news from the runner.
    Lost,
}

impl ResultArg {
    /// The job state it names.
    pub fn state(self) -> JobState {
        match self {
            ResultArg::Queued => JobState::Queued,
            ResultArg::Running => JobState::Running,
            ResultArg::Cancelling => JobState::Cancelling,
            ResultArg::Passed => JobState::Passed,
            ResultArg::Failed => JobState::Failed,
            ResultArg::Timeout => JobState::Timeout,
            ResultArg::Cancelled | ResultArg::Rejected => JobState::Cancelled,
            ResultArg::Lost => JobState::Lost,
        }
    }
}

/// `explain <job>`.
#[derive(Args, Debug)]
pub struct ExplainArgs {
    /// The job.
    pub job: String,
    /// The client key whose run to explain, when several clients use the same job id.
    #[arg(long)]
    pub client: Option<String>,
    /// Only this broker.
    #[arg(long)]
    pub broker: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

/// Filters of `history`.
#[derive(Args, Debug)]
pub struct HistoryFilters {
    /// Only this project.
    #[arg(long)]
    pub project: Option<String>,
    /// Only this source.
    #[arg(long, value_enum)]
    pub source: Option<SourceArg>,
    /// Only this label.
    #[arg(long = "as", value_name = "LABEL")]
    pub label: Option<String>,
    /// Only this profile.
    #[arg(long)]
    pub profile: Option<String>,
    /// Only runs that ended (or stand) so.
    #[arg(long, value_enum)]
    pub result: Option<ResultArg>,
    /// Only runs queued since: an age (`30m`, `2h`, `7d`) or a UTC date (`2026-11-01`). It is
    /// the enqueue time; for what ran in a window use `--ran-since` and `--ran-until`.
    #[arg(long, value_parser = parse_since)]
    pub since: Option<Since>,
    /// Only runs that were running at some moment since: started, and not finished before it (a
    /// run still going counts, an order that never started does not). An age or a UTC date.
    #[arg(long, value_parser = parse_since, value_name = "SINCE")]
    pub ran_since: Option<Since>,
    /// Only runs that started at or before this time. With `--ran-since` the pair is a window:
    /// the runs that overlap it. An age or a UTC date.
    #[arg(long, value_parser = parse_since, value_name = "UNTIL")]
    pub ran_until: Option<Since>,
    /// At most this many runs.
    #[arg(long)]
    pub limit: Option<u64>,
    /// Only this platform.
    #[arg(long)]
    pub platform: Option<String>,
    /// Only this runner.
    #[arg(long)]
    pub runner: Option<String>,
    /// Runs of this client key instead of this client's own.
    #[arg(long, conflicts_with = "all")]
    pub client: Option<String>,
    /// Runs of every client the broker's visibility policy shows.
    #[arg(long)]
    pub all: bool,
    /// Only this broker.
    #[arg(long)]
    pub broker: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

/// `history [filters] | history show <job>`.
#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct HistoryArgs {
    /// `show <job>`.
    #[command(subcommand)]
    pub action: Option<HistoryAction>,
    /// Filters of the list.
    #[command(flatten)]
    pub filters: HistoryFilters,
}

/// `history show <job>`.
#[derive(Args, Debug)]
pub struct ShowArgs {
    /// The job.
    pub job: String,
    /// Only this broker.
    #[arg(long)]
    pub broker: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

/// What `history` does besides listing.
#[derive(Subcommand, Debug)]
pub enum HistoryAction {
    /// The full record of one run.
    Show(ShowArgs),
}

/// `log <job>`.
#[derive(Args, Debug)]
pub struct LogArgs {
    /// The job.
    pub job: String,
    /// Accepted like on `submit`, `status` and `jobs` so a session can pass its label to every
    /// command; the log is read under the broker's visibility policy and the label is not used.
    #[arg(long = "as", value_name = "LABEL")]
    pub label: Option<String>,
    /// Only this step.
    #[arg(long)]
    pub step: Option<String>,
    /// Only the last N lines.
    #[arg(long)]
    pub tail: Option<u64>,
    /// Keep following the log of a running job until it ends.
    #[arg(short = 'f', long)]
    pub follow: bool,
    /// Only this broker.
    #[arg(long)]
    pub broker: Option<String>,
    /// Print JSON lines `{job, text, end: false}`, then `{job, broker, end: true}`.
    #[arg(long)]
    pub json: bool,
}

/// `stats`.
#[derive(Args, Debug)]
pub struct StatsArgs {
    /// Only this project.
    #[arg(long)]
    pub project: Option<String>,
    /// Only this profile.
    #[arg(long)]
    pub profile: Option<String>,
    /// Only this client key (the broker owner sees every client, others only their own).
    #[arg(long)]
    pub client: Option<String>,
    /// Only runs queued since an age or a UTC date.
    #[arg(long, value_parser = parse_since)]
    pub since: Option<Since>,
    /// Only this broker.
    #[arg(long)]
    pub broker: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

/// `runners`.
#[derive(Args, Debug)]
pub struct RunnersArgs {
    /// Show everything known about the runner of this name instead of the table.
    pub name: Option<String>,
    /// Add the programs, enforcement and cache columns.
    #[arg(long)]
    pub wide: bool,
    /// Only this broker.
    #[arg(long)]
    pub broker: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

/// `status --html [path] [--history] [--with-logs]` (spec 9.1, 9.4).
#[derive(Args, Debug)]
pub struct SnapshotArgs {
    /// Write the dashboard as one self-contained HTML file (default `temp/runner/queue.html`).
    #[arg(long, value_name = "PATH", num_args = 0..=1)]
    pub html: Option<Option<PathBuf>>,
    /// Add the history table to the snapshot.
    #[arg(long, requires = "html")]
    pub history: bool,
    /// Embed the logs the brokers let this client read; logs may hold secrets of profile
    /// commands, so they are left out without this flag.
    #[arg(long, requires = "html")]
    pub with_logs: bool,
}
