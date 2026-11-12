//! `status --html [path] [--history] [--with-logs]` (spec 9.1, 9.4): the dashboard as one
//! self-contained HTML file with the data inline. Finished runs are added only with
//! `--history`; logs only with `--with-logs`, and then only those the brokers give this client,
//! read through the hub and masked by the secret redactor.

use super::jobs::hub;
use super::log_cmd::{cli_redactor, find_log, LogQuery};
use super::{SnapshotArgs, SourceArg};
use crate::dashboard::build_snapshot;
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_core::JobId;
use lr_obs::Redactor;
use lr_proto::{DashboardList, DashboardSnapshot, StatusFilter, WireSource};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Lines of each log a snapshot embeds with `--with-logs`.
pub const SNAPSHOT_LOG_LINES: u64 = 500;

/// Where `status --html` writes when no path is given.
pub fn default_path() -> PathBuf {
    PathBuf::from("temp").join("runner").join("queue.html")
}

/// The jobs whose logs `--with-logs` asks for: running and finished ones, with their broker.
pub fn log_targets(snapshot: &DashboardSnapshot) -> Vec<(String, JobId)> {
    snapshot
        .jobs
        .iter()
        .filter(|j| matches!(j.list, DashboardList::Running | DashboardList::History))
        .filter_map(|j| Some((j.broker.clone()?, j.detail.summary.job.clone())))
        .collect()
}

/// The logs of `targets` this client may read, masked by `redactor`; refused and missing logs
/// are left out.
pub async fn readable_logs(
    client: &IpcClient,
    targets: &[(String, JobId)],
    redactor: &Arc<Redactor>,
) -> Vec<(JobId, String)> {
    let mut out = Vec::new();
    for (broker, job) in targets {
        let q = LogQuery {
            job: job.clone(),
            step: None,
            tail: Some(SNAPSHOT_LOG_LINES),
            follow: false,
        };
        let mut text = String::new();
        if find_log(client, Some(broker), &q, redactor, &mut |t| {
            text.push_str(t)
        })
        .await
        .is_ok()
        {
            out.push((job.clone(), text));
        }
    }
    out
}

/// Writes a snapshot readable by the current OS user only (`0600` on Unix, a DACL of the
/// owner on Windows): with `--with-logs` it carries logs.
pub fn save(path: &Path, html: &str) -> Result<(), String> {
    lr_net::write_private(path, html.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}

/// `status --html`: writes the snapshot and prints its path.
pub async fn write(
    layout: &Layout,
    project: Option<String>,
    source: Option<SourceArg>,
    label: Option<String>,
    path: Option<PathBuf>,
    args: &SnapshotArgs,
) -> Result<i32, String> {
    let filter = StatusFilter {
        project,
        source: source.map(|s| match s {
            SourceArg::Session => WireSource::Session,
            SourceArg::Github => WireSource::Github,
        }),
        label,
        ..StatusFilter::default()
    };
    let client = hub(layout).await?;
    let redactor = cli_redactor(layout);
    let snapshot = build_snapshot(&client, &filter, args.history, &redactor).await;
    let snapshot = match snapshot {
        Ok(s) => s,
        Err(e) => {
            client.close().await;
            return Err(e);
        }
    };
    let logs = match args.with_logs {
        true => readable_logs(&client, &log_targets(&snapshot), &redactor).await,
        false => Vec::new(),
    };
    client.close().await;
    let html = lr_dashboard::render_snapshot(&snapshot, &logs);
    let path = path.unwrap_or_else(default_path);
    save(&path, &html)?;
    println!("{}", path.display());
    Ok(0)
}
