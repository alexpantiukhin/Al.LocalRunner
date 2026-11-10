/// A receiver of metric updates (spec 12.2), implemented by `lr-obs`.
///
/// Names are the full exported names (`localrunner_queue_length`, ...) and labels are given as
/// `(key, value)` pairs. An update the receiver does not accept (unknown name, wrong kind,
/// label set not in the catalog, too many series) is dropped and counted, never an error for
/// the caller, so recording a metric never stops or slows a role.
pub trait Meter: Send + Sync {
    /// Adds `delta` to a counter, saturating.
    fn add(&self, name: &str, labels: &[(&str, &str)], delta: u64);

    /// Sets a gauge to `value`.
    fn set(&self, name: &str, labels: &[(&str, &str)], value: f64);

    /// Records one observation of a histogram.
    fn observe(&self, name: &str, labels: &[(&str, &str)], value: f64);
}
