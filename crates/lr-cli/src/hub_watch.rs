//! How `dashboard` learns that `down` stopped the hub it serves (spec 9.1): it checks the hub's
//! CLI channel every second and ends once a hub that answered misses two checks in a row.

use crate::commands::jobs::hub;
use crate::paths::Layout;
use std::future::Future;
use std::time::Duration;

/// How often `dashboard` checks that the hub it serves still answers.
pub const HUB_CHECK: Duration = Duration::from_secs(1);

/// Missed checks in a row after which the hub counts as stopped.
pub const MISSES: u32 = 2;

/// Resolves once `answers`, asked every `every`, said yes and then no [`MISSES`] times in a
/// row; a yes in between starts the count again. Never while it has not said yes at all.
pub async fn stopped_after_misses<F, Fut>(mut answers: F, every: Duration)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let (mut seen, mut misses) = (false, 0);
    loop {
        if answers().await {
            seen = true;
            misses = 0;
        } else if seen {
            misses += 1;
            if misses >= MISSES {
                return;
            }
        }
        tokio::time::sleep(every).await;
    }
}

/// Resolves once the hub of `layout` stops answering on its CLI channel after it answered
/// (`down` stopped it), checking every `every`; never while the hub was not seen at all.
pub async fn hub_stopped(layout: &Layout, every: Duration) {
    let answers = || async {
        match hub(layout).await {
            Ok(client) => {
                client.close().await;
                true
            }
            Err(_) => false,
        }
    };
    stopped_after_misses(answers, every).await
}
