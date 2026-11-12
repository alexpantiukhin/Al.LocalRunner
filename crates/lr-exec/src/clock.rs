use lr_abstractions::{Clock, Sleep};
use lr_core::Timestamp;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The real clock: wall-clock time of the OS and Tokio timers.
///
/// It lives in `lr-exec` because this is the one crate of the I/O layer that already depends
/// on the Tokio timer; roles receive it as `Arc<dyn Clock>` like any other clock. `sleep`
/// must be called inside a Tokio runtime with the timer enabled.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        Timestamp(since.as_millis() as u64)
    }

    fn sleep(&self, duration: Duration) -> Sleep {
        Box::pin(tokio::time::sleep(duration))
    }
}
