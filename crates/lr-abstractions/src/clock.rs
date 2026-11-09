use lr_core::Timestamp;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// A sleep in progress; completes when its deadline has passed on the clock that made it.
pub type Sleep = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Source of time for every role: no role reads the system time or uses a timer directly, so
/// deadlines (`lost`, `ttl_hours`, the reaper period, heartbeats) are tested by substituting a
/// manual clock (spec 13.1).
pub trait Clock: Send + Sync {
    /// Current wall-clock time in unix milliseconds.
    fn now(&self) -> Timestamp;

    /// Completes once `duration` has elapsed on this clock, counted from this call.
    fn sleep(&self, duration: Duration) -> Sleep;

    /// Completes once this clock reaches `at`; at once when `at` is not in the future.
    fn sleep_until(&self, at: Timestamp) -> Sleep {
        self.sleep(Duration::from_millis(at.millis_since(self.now())))
    }
}
