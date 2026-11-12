//! The broker inside the OS service (spec 10): a broker that did not start or ended on its own
//! is a failure for the service manager, which restarts the service after 5 seconds, and the
//! reason goes to `logs/service.log` of the service's state directory.

use crate::paths::Layout;
use std::future::Future;

/// How the broker of a service ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceEnd {
    /// The service manager stopped it.
    Stopped,
    /// The broker did not start; the reason is in `logs/service.log`.
    NotStarted,
    /// The broker ended without being asked; noted in `logs/service.log`.
    Ended,
}

/// Appends a line `<unix seconds> <text>` to `logs/service.log` of `layout`; a text of several
/// lines is joined into one with `; `.
pub fn service_log(layout: &Layout, text: &str) {
    let text = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    let dir = layout.root.join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("service.log"))
    {
        let _ = writeln!(f, "{at} {text}");
    }
}

/// Runs the broker of `layout` until `stop` completes; `started` is called once it runs.
pub async fn serve(
    layout: &Layout,
    started: impl FnOnce(),
    stop: impl Future<Output = ()>,
) -> ServiceEnd {
    let running = match crate::roles::start_broker(layout, false).await {
        Ok(r) => r,
        Err(e) => {
            service_log(layout, &format!("the broker did not start: {e}"));
            return ServiceEnd::NotStarted;
        }
    };
    started();
    tokio::select! {
        _ = stop => ServiceEnd::Stopped,
        _ = running.wait() => {
            service_log(layout, "the broker ended on its own");
            ServiceEnd::Ended
        }
    }
}
