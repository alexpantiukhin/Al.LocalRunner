//! `dashboard` (spec 9): the loopback page over a [`DataSource`] that reads the hub through its
//! CLI channel and builds the [`DashboardSnapshot`] model from the brokers' answers. The URL
//! carries the launch token, so it is printed to the terminal only and never logged.

use crate::commands::jobs::hub;
use crate::commands::log_cmd::cli_redactor;
use crate::commands::view::{known_brokers, read};
pub use crate::dashboard_model::{
    add_hub_orders, build_snapshot, detail_of, finished_runs, hub_orders, keep_last_known,
    HISTORY_LIMIT, ORDERS_WAIT,
};
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use async_trait::async_trait;
use lr_abstractions::{CancelRequest, DataError, DataSource, EventStream};
use lr_core::JobId;
use lr_dashboard::{Dashboard, DashboardOptions, LOG_TAIL_BYTES};
use lr_obs::Redactor;
use lr_proto::{
    CancelTarget, CliReply, CliRequest, DashboardEvent, DashboardSnapshot, QueueEvent, ReadReply,
    ReadRequest, StatusFilter,
};
use std::sync::Arc;
use std::time::Duration;

/// How often the page is told to refresh when the hub does not relay its brokers' events (a
/// hub older than [`CliRequest::Events`]).
pub const REFRESH: Duration = Duration::from_secs(2);

/// How long the dashboard waits for the hub to open its event stream.
pub const EVENTS_WAIT: Duration = Duration::from_secs(5);

/// The dashboard's data from the hub of this OS user.
pub struct HubSource {
    layout: Layout,
    last: std::sync::Mutex<Option<DashboardSnapshot>>,
}

impl HubSource {
    /// Data from the hub of `layout`.
    pub fn new(layout: Layout) -> HubSource {
        HubSource {
            layout,
            last: std::sync::Mutex::new(None),
        }
    }
}

fn unavailable(e: impl ToString) -> DataError {
    DataError::Unavailable(e.to_string())
}

/// The last [`LOG_TAIL_BYTES`] of a log copy as whole lines masked by `redactor`: a line cut by
/// the start of the tail is left out, so a secret it carried cannot pass half-masked.
pub fn masked_tail(bytes: &[u8], redactor: &Redactor) -> Vec<u8> {
    let start = bytes.len().saturating_sub(LOG_TAIL_BYTES);
    let mut tail = &bytes[start..];
    if start > 0 && bytes[start - 1] != b'\n' {
        let skip = tail
            .iter()
            .position(|b| *b == b'\n')
            .map_or(tail.len(), |i| i + 1);
        tail = &tail[skip..];
    }
    redactor
        .redact_text(&String::from_utf8_lossy(tail))
        .into_bytes()
}

struct Ticks;

#[async_trait]
impl EventStream<DashboardEvent> for Ticks {
    async fn next(&mut self) -> Option<DashboardEvent> {
        tokio::time::sleep(REFRESH).await;
        Some(DashboardEvent {
            broker: "hub".into(),
            event: QueueEvent::Unknown,
        })
    }
}

struct HubEvents(IpcClient);

#[async_trait]
impl EventStream<DashboardEvent> for HubEvents {
    async fn next(&mut self) -> Option<DashboardEvent> {
        loop {
            match self.0.recv::<CliReply>().await.ok()? {
                CliReply::Event(e) => return Some(e),
                CliReply::Error(_) => return None,
                _ => {}
            }
        }
    }
}

/// The live events of the hub of `layout` (spec 9.3): its brokers' queue and runner events,
/// each masked by its broker for this hub's key. A hub that does not open the stream within
/// [`EVENTS_WAIT`] (one older than [`CliRequest::Events`] ignores it) gives refresh ticks
/// every [`REFRESH`] instead.
pub async fn hub_events(
    layout: &Layout,
) -> Result<Box<dyn EventStream<DashboardEvent>>, DataError> {
    let client = hub(layout).await.map_err(unavailable)?;
    let opened = tokio::time::timeout(EVENTS_WAIT, client.call::<_, CliReply>(&CliRequest::Events));
    match opened.await {
        Ok(Ok(CliReply::Watching)) => Ok(Box::new(HubEvents(client))),
        Ok(Err(e)) => Err(unavailable(e)),
        _ => {
            client.close().await;
            Ok(Box::new(Ticks))
        }
    }
}

/// The log copy of `job` for the page: from the directory of the run a reachable broker
/// knows, else the copy the hub keeps for its own active order; masked by `redactor`.
pub async fn log_copy(
    client: &IpcClient,
    job: &JobId,
    redactor: &Redactor,
) -> Result<Vec<u8>, DataError> {
    let safe = !job.0.is_empty()
        && job.0.len() <= 64
        && job.0.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !safe {
        return Err(DataError::NotFound);
    }
    let brokers = known_brokers(client).await.map_err(unavailable)?;
    for b in brokers.iter().filter(|b| b.available) {
        let req = ReadRequest::HistoryShow {
            job: job.clone(),
            client: None,
        };
        if let Ok(ReadReply::Job(d)) = read(client, &b.name, req).await {
            let Some(cwd) = d.cwd else {
                return Err(DataError::NotFound);
            };
            let dir = std::path::Path::new(&cwd);
            if !clean_absolute(dir) {
                return Err(DataError::NotFound);
            }
            let path = dir.join("temp").join("runner").join(format!("{job}.log"));
            let bytes = std::fs::read(&path).map_err(|_| DataError::NotFound)?;
            return Ok(masked_tail(&bytes, redactor));
        }
    }
    let order = hub_orders(client)
        .await
        .into_iter()
        .find(|o| &o.summary.job == job);
    let Some(path) = order.and_then(|o| o.log) else {
        return Err(DataError::NotFound);
    };
    let path = std::path::Path::new(&path);
    let named = path.file_name() == Some(std::ffi::OsStr::new(&format!("{job}.log")));
    if !named || !clean_absolute(path) {
        return Err(DataError::NotFound);
    }
    let bytes = std::fs::read(path).map_err(|_| DataError::NotFound)?;
    Ok(masked_tail(&bytes, redactor))
}

fn clean_absolute(path: &std::path::Path) -> bool {
    path.is_absolute()
        && path.components().all(|c| {
            !matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
}

#[async_trait]
impl DataSource for HubSource {
    type Snapshot = DashboardSnapshot;
    type Event = DashboardEvent;

    async fn snapshot(&self) -> Result<DashboardSnapshot, DataError> {
        let client = hub(&self.layout).await.map_err(unavailable)?;
        let redactor = cli_redactor(&self.layout);
        let snap = build_snapshot(&client, &StatusFilter::default(), true, &redactor).await;
        client.close().await;
        let mut snap = snap.map_err(unavailable)?;
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        keep_last_known(&mut snap, last.as_ref());
        *last = Some(snap.clone());
        Ok(snap)
    }

    async fn subscribe(&self) -> Result<Box<dyn EventStream<DashboardEvent>>, DataError> {
        hub_events(&self.layout).await
    }

    async fn log(&self, job: &JobId) -> Result<Vec<u8>, DataError> {
        let client = hub(&self.layout).await.map_err(unavailable)?;
        let copy = log_copy(&client, job, &cli_redactor(&self.layout)).await;
        client.close().await;
        copy
    }

    async fn cancel(&self, job: &JobId, request: CancelRequest) -> Result<(), DataError> {
        let cwd = std::env::current_dir().map_err(unavailable)?;
        let actor =
            crate::facts::collect_session(&request.actor, None, &cwd).map_err(unavailable)?;
        let client = hub(&self.layout).await.map_err(unavailable)?;
        let req = CliRequest::Cancel(lr_proto::CancelRequest {
            actor,
            target: CancelTarget::Job(job.clone()),
            force: true,
            reason: request.reason,
        });
        match client
            .call::<_, CliReply>(&req)
            .await
            .map_err(unavailable)?
        {
            CliReply::Cancelled { .. } => Ok(()),
            CliReply::Error(e) => Err(DataError::Refused(e.message)),
            other => Err(DataError::Refused(format!("{other:?}"))),
        }
    }
}

/// `dashboard`: serves until Ctrl+C or until `down` stops the hub it serves. The URL goes to
/// stdout only.
pub async fn run(layout: &Layout) -> Result<i32, String> {
    let user = lr_net::current_os_user().map_err(|e| e.to_string())?;
    let source = Arc::new(HubSource::new(layout.clone()));
    let dashboard = Dashboard::new(source, DashboardOptions { user }).map_err(|e| e.to_string())?;
    let running = dashboard.serve().await.map_err(|e| e.to_string())?;
    println!("{}", running.url());
    eprintln!("dashboard on loopback; Ctrl+C or down stops it");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = crate::hub_watch::hub_stopped(layout, crate::hub_watch::HUB_CHECK) => eprintln!("the hub stopped; dashboard ends"),
    }
    running.shutdown().await;
    Ok(0)
}
