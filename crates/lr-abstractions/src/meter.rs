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

    /// Forgets every series whose label `label` equals `value` (a runner, service or pool
    /// that is gone), so series of ended entities do not live forever.
    fn forget(&self, label: &str, value: &str) {
        let _ = (label, value);
    }
}

/// Names and label keys of the metrics the roles report (spec 12.2), shared by the roles and
/// the `lr-obs` catalog.
pub mod metric {
    /// Orders waiting in the broker queue, by `platform` (gauge).
    pub const QUEUE_LENGTH: &str = "localrunner_queue_length";
    /// Time from queueing to assignment, by `platform` (histogram, s).
    pub const QUEUE_WAIT_SECONDS: &str = "localrunner_queue_wait_seconds";
    /// Jobs running on a runner, by `pool` and `runner` (gauge).
    pub const RUNNING_JOBS: &str = "localrunner_running_jobs";
    /// Pool memory taken by the jobs of a runner, by `pool` and `runner` (gauge, MB).
    pub const POOL_MEMORY_USED_MB: &str = "localrunner_pool_memory_used_mb";
    /// Pool CPU taken by the jobs of a runner, by `pool` and `runner` (gauge).
    pub const POOL_CPU_USED: &str = "localrunner_pool_cpu_used";
    /// Longest current wait of a queued order, by saturation `reason` (gauge, s).
    pub const SATURATION_WAIT_SECONDS: &str = "localrunner_saturation_wait_seconds";
    /// Orders ended or refused, by `result` and `platform` (counter).
    pub const ORDERS_TOTAL: &str = "localrunner_orders_total";
    /// Time from start to the final state, by `result` and `platform` (histogram, s).
    pub const JOB_DURATION_SECONDS: &str = "localrunner_job_duration_seconds";
    /// 1 while a runner is connected, 0 otherwise, by `runner` (gauge).
    pub const RUNNER_ONLINE: &str = "localrunner_runner_online";
    /// Size of the named caches of a runner, by `runner` (gauge, MB).
    pub const CACHE_USED_MB: &str = "localrunner_cache_used_mb";
    /// Cache entries removed, by `runner` and `reason` (`size`, `age`) (counter).
    pub const CACHE_EVICTED_TOTAL: &str = "localrunner_cache_evicted_total";
    /// 1 while a runner service is ready, 0 once it is gone, by `runner` and `service`
    /// (gauge).
    pub const SERVICE_UP: &str = "localrunner_service_up";
    /// Start attempts of a runner service, by `runner` and `service` (counter).
    pub const SERVICE_STARTS_TOTAL: &str = "localrunner_service_starts_total";
    /// Failures of a runner service (start, readiness, crash), by `runner` and `service`
    /// (counter).
    pub const SERVICE_FAILURES_TOTAL: &str = "localrunner_service_failures_total";
    /// Messages waiting in the outbox of a role, by `role` (gauge).
    pub const OUTBOX_PENDING: &str = "localrunner_outbox_pending";
    /// Age of the oldest waiting outbox message, by `role` (gauge, s).
    pub const OUTBOX_OLDEST_AGE_SECONDS: &str = "localrunner_outbox_oldest_age_seconds";
    /// Policy reloads, by `result` (`ok`, `error`) (counter).
    pub const POLICY_RELOAD_TOTAL: &str = "localrunner_policy_reload_total";

    /// Every name above.
    pub const ALL: &[&str] = &[
        QUEUE_LENGTH,
        QUEUE_WAIT_SECONDS,
        RUNNING_JOBS,
        POOL_MEMORY_USED_MB,
        POOL_CPU_USED,
        SATURATION_WAIT_SECONDS,
        ORDERS_TOTAL,
        JOB_DURATION_SECONDS,
        RUNNER_ONLINE,
        CACHE_USED_MB,
        CACHE_EVICTED_TOTAL,
        SERVICE_UP,
        SERVICE_STARTS_TOTAL,
        SERVICE_FAILURES_TOTAL,
        OUTBOX_PENDING,
        OUTBOX_OLDEST_AGE_SECONDS,
        POLICY_RELOAD_TOTAL,
    ];

    /// Label: platform of an order (`same_machine` for a job bound to the client machine).
    pub const PLATFORM: &str = "platform";
    /// Label: the pool of the broker.
    pub const POOL: &str = "pool";
    /// Label: runner name.
    pub const RUNNER: &str = "runner";
    /// Label: reason of a wait or an eviction.
    pub const REASON: &str = "reason";
    /// Label: result of an order or a reload.
    pub const RESULT: &str = "result";
    /// Label: service name, only names declared in `runner.toml`.
    pub const SERVICE: &str = "service";
    /// Label: role of the process.
    pub const ROLE: &str = "role";
    /// Label: client label, kept only when `[metrics] client_label = true`.
    pub const CLIENT: &str = "client";

    /// The `pool` value: a broker serves exactly one pool (spec 2).
    pub const POOL_VALUE: &str = "default";
    /// The `platform` value of an order without a platform (a same-machine job).
    pub const SAME_MACHINE: &str = "same_machine";
    /// The `platform` value of a refused order whose platform no known runner offers, so
    /// arbitrary client text never becomes a series.
    pub const OTHER_PLATFORM: &str = "other";
    /// How often a role samples its outbox, in seconds.
    pub const OUTBOX_SAMPLE_S: u64 = 15;
}
