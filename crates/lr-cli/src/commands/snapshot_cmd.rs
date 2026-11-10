//! `status --html [path] [--history] [--with-logs]` (spec 9.1, 9.4): the dashboard as one
//! self-contained HTML file with the data inline. Finished runs are added only with
//! `--history`; logs only with `--with-logs`, and then only those the brokers give this client,
//! read through the hub and masked by the secret redactor.

use super::jobs::hub;
use super::log_cmd::{find_log, LogQuery};
use super::{SnapshotArgs, SourceArg};
use crate::dashboard::build_snapshot;
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_core::JobId;
use lr_proto::{DashboardList, DashboardSnapshot, StatusFilter, WireSource};
use std::path::PathBuf;

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

/// The logs of `targets` this client may read; refused and missing logs are left out.
pub async fn readable_logs(
    client: &IpcClient,
    targets: &[(String, JobId)],
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
        if find_log(client, Some(broker), &q, &mut |t| text.push_str(t))
            .await
            .is_ok()
        {
            out.push((job.clone(), text));
        }
    }
    out
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
    let snapshot = build_snapshot(&client, &filter, args.history).await;
    let snapshot = match snapshot {
        Ok(s) => s,
        Err(e) => {
            client.close().await;
            return Err(e);
        }
    };
    let logs = match args.with_logs {
        true => readable_logs(&client, &log_targets(&snapshot)).await,
        false => Vec::new(),
    };
    client.close().await;
    let html = lr_dashboard::render_snapshot(&snapshot, &logs);
    let path = path.unwrap_or_else(default_path);
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, html).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("{}", path.display());
    Ok(0)
}
