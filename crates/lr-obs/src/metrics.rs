//! Metrics of spec 12.2: a fixed catalog, kept in process memory and published only through
//! an enabled OTLP sink.
//!
//! Cardinality stays bounded: only catalog names and label sets are accepted, label values
//! the spec enumerates must come from that list, a value is at most
//! [`MAX_LABEL_VALUE_BYTES`] printable bytes, and a metric has at most
//! [`MAX_SERIES_PER_METRIC`] series. Project, branch, client label and job names are never
//! labels; the `client` label exists only with `[metrics] client_label = true` and only on
//! [`CLIENT_LABEL_METRICS`]. Refused updates are counted in [`Metrics::rejected`].

use lr_abstractions::Meter;
use lr_core::Timestamp;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// Kind of a metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Monotonic, cumulative since process start.
    Counter,
    /// Last value set.
    Gauge,
    /// Distribution over [`HISTOGRAM_BOUNDS_S`].
    Histogram,
}

/// One catalog entry.
#[derive(Debug)]
pub struct MetricDef {
    /// Full exported name.
    pub name: &'static str,
    /// Kind.
    pub kind: Kind,
    /// UCUM unit.
    pub unit: &'static str,
    /// Exact label keys, in export order.
    pub labels: &'static [&'static str],
    /// Roles that report it.
    pub roles: &'static [&'static str],
    /// Labels whose values are restricted, with the allowed values.
    pub values: &'static [(&'static str, &'static [&'static str])],
}

/// Common prefix of every name.
pub const PREFIX: &str = "localrunner_";

const BROKER: &[&str] = &["broker"];
const RUNNER: &[&str] = &["runner"];
const ALL: &[&str] = &["hub", "broker", "runner"];
const RESULTS: &[&str] = &[
    "passed",
    "failed",
    "timeout",
    "cancelled",
    "lost",
    "rejected",
];

const fn def(
    name: &'static str,
    kind: Kind,
    unit: &'static str,
    labels: &'static [&'static str],
    roles: &'static [&'static str],
    values: &'static [(&'static str, &'static [&'static str])],
) -> MetricDef {
    MetricDef {
        name,
        kind,
        unit,
        labels,
        roles,
        values,
    }
}

use Kind::{Counter, Gauge, Histogram};

/// Every metric of spec 12.2.
pub const CATALOG: &[MetricDef] = &[
    def(
        "localrunner_queue_length",
        Gauge,
        "{order}",
        &["platform"],
        BROKER,
        &[],
    ),
    def(
        "localrunner_queue_wait_seconds",
        Histogram,
        "s",
        &["platform"],
        BROKER,
        &[],
    ),
    def(
        "localrunner_running_jobs",
        Gauge,
        "{job}",
        &["pool", "runner"],
        BROKER,
        &[],
    ),
    def(
        "localrunner_pool_memory_used_mb",
        Gauge,
        "MBy",
        &["pool", "runner"],
        BROKER,
        &[],
    ),
    def(
        "localrunner_pool_cpu_used",
        Gauge,
        "{cpu}",
        &["pool", "runner"],
        BROKER,
        &[],
    ),
    def(
        "localrunner_saturation_wait_seconds",
        Gauge,
        "s",
        &["reason"],
        BROKER,
        &[("reason", &["runner_busy", "memory", "cpu", "tag", "rule"])],
    ),
    def(
        "localrunner_orders_total",
        Counter,
        "{order}",
        &["result", "platform"],
        BROKER,
        &[("result", RESULTS)],
    ),
    def(
        "localrunner_job_duration_seconds",
        Histogram,
        "s",
        &["result", "platform"],
        BROKER,
        &[("result", RESULTS)],
    ),
    def(
        "localrunner_runner_online",
        Gauge,
        "1",
        &["runner"],
        BROKER,
        &[],
    ),
    def(
        "localrunner_cache_used_mb",
        Gauge,
        "MBy",
        &["runner"],
        RUNNER,
        &[],
    ),
    def(
        "localrunner_cache_evicted_total",
        Counter,
        "{entry}",
        &["runner", "reason"],
        RUNNER,
        &[("reason", &["size", "age"])],
    ),
    def(
        "localrunner_service_up",
        Gauge,
        "1",
        &["runner", "service"],
        RUNNER,
        &[],
    ),
    def(
        "localrunner_service_starts_total",
        Counter,
        "{start}",
        &["runner", "service"],
        RUNNER,
        &[],
    ),
    def(
        "localrunner_service_failures_total",
        Counter,
        "{failure}",
        &["runner", "service"],
        RUNNER,
        &[],
    ),
    def(
        "localrunner_outbox_pending",
        Gauge,
        "{message}",
        &["role"],
        ALL,
        &[("role", ALL)],
    ),
    def(
        "localrunner_outbox_oldest_age_seconds",
        Gauge,
        "s",
        &["role"],
        ALL,
        &[("role", ALL)],
    ),
    def(
        "localrunner_policy_reload_total",
        Counter,
        "{reload}",
        &["result"],
        BROKER,
        &[("result", &["ok", "error"])],
    ),
    def(
        "localrunner_log_dropped_total",
        Counter,
        "{event}",
        &["sink"],
        ALL,
        &[("sink", &["file", "console", "journal", "otlp"])],
    ),
];

/// Upper bounds of histogram buckets, in seconds; one more bucket counts larger values.
pub const HISTOGRAM_BOUNDS_S: &[f64] = &[
    1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0, 7200.0, 14400.0,
];

/// Most series one metric keeps; updates of new series beyond it are refused.
pub const MAX_SERIES_PER_METRIC: usize = 256;

/// Longest label value accepted, in bytes.
pub const MAX_LABEL_VALUE_BYTES: usize = 128;

/// The optional client label (spec 12.2: only by explicit setting).
pub const CLIENT_LABEL: &str = "client";

/// Metrics that may carry [`CLIENT_LABEL`] when it is enabled.
pub const CLIENT_LABEL_METRICS: &[&str] = &[
    "localrunner_queue_wait_seconds",
    "localrunner_orders_total",
    "localrunner_job_duration_seconds",
];

/// Current value of one series.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Counter total.
    Counter(u64),
    /// Gauge value.
    Gauge(f64),
    /// Histogram state.
    Histogram {
        /// Observations.
        count: u64,
        /// Sum of observations.
        sum: f64,
        /// Count per bucket (not cumulative), one more than [`HISTOGRAM_BOUNDS_S`].
        buckets: Vec<u64>,
    },
}

/// One series in a snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    /// Full name.
    pub name: &'static str,
    /// Kind.
    pub kind: Kind,
    /// Unit.
    pub unit: &'static str,
    /// Labels in catalog order (`client` last when present).
    pub labels: Vec<(String, String)>,
    /// Value.
    pub value: Value,
}

type Series = BTreeMap<Vec<(String, String)>, Value>;

/// The metric registry of a process; implements [`Meter`] for the roles.
pub struct Metrics {
    start: Timestamp,
    client_label: AtomicBool,
    series: Mutex<Vec<Series>>,
    rejected: AtomicU64,
}

enum Update {
    Add(u64),
    Set(f64),
    Observe(f64),
}

fn valid_value(v: &str) -> bool {
    !v.is_empty() && v.len() <= MAX_LABEL_VALUE_BYTES && !v.chars().any(char::is_control)
}

impl Metrics {
    /// An empty registry; counters are cumulative from `start`.
    pub fn new(start: Timestamp, client_label: bool) -> Metrics {
        Metrics {
            start,
            client_label: AtomicBool::new(client_label),
            series: Mutex::new(CATALOG.iter().map(|_| Series::new()).collect()),
            rejected: AtomicU64::new(0),
        }
    }

    /// Turns the `client` label on or off (`[metrics] client_label`).
    pub fn set_client_label(&self, on: bool) {
        self.client_label.store(on, Ordering::Relaxed);
    }

    /// Start of the cumulative counters.
    pub fn start(&self) -> Timestamp {
        self.start
    }

    /// Updates refused so far.
    pub fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    /// Every series, in catalog order, then by labels.
    pub fn snapshot(&self) -> Vec<Point> {
        let series = self.series.lock().unwrap_or_else(|e| e.into_inner());
        CATALOG
            .iter()
            .zip(series.iter())
            .flat_map(|(def, s)| {
                s.iter().map(move |(labels, value)| Point {
                    name: def.name,
                    kind: def.kind,
                    unit: def.unit,
                    labels: labels.clone(),
                    value: value.clone(),
                })
            })
            .collect()
    }

    fn labels(&self, def: &MetricDef, given: &[(&str, &str)]) -> Option<Vec<(String, String)>> {
        let client_ok =
            self.client_label.load(Ordering::Relaxed) && CLIENT_LABEL_METRICS.contains(&def.name);
        let mut out = Vec::with_capacity(def.labels.len() + 1);
        for key in def.labels {
            let mut found = given.iter().filter(|(k, _)| k == key);
            let (_, value) = found.next()?;
            if found.next().is_some() || !valid_value(value) {
                return None;
            }
            if let Some((_, allowed)) = def.values.iter().find(|(l, _)| l == key) {
                if !allowed.contains(value) {
                    return None;
                }
            }
            out.push((key.to_string(), value.to_string()));
        }
        let mut client = given.iter().filter(|(k, _)| *k == CLIENT_LABEL);
        let first_client = client.next();
        if client.next().is_some() {
            return None;
        }
        if let Some((_, value)) = first_client {
            if CLIENT_LABEL_METRICS.contains(&def.name) {
                if client_ok {
                    if !valid_value(value) {
                        return None;
                    }
                    out.push((CLIENT_LABEL.to_string(), value.to_string()));
                }
            } else if self.client_label.load(Ordering::Relaxed) {
                return None;
            }
        }
        let known = def.labels.len() + usize::from(first_client.is_some());
        (given.len() == known).then_some(out)
    }

    fn update(&self, name: &str, given: &[(&str, &str)], update: Update) {
        if !self.apply(name, given, update) {
            self.rejected.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn apply(&self, name: &str, given: &[(&str, &str)], update: Update) -> bool {
        let Some(index) = CATALOG.iter().position(|d| d.name == name) else {
            return false;
        };
        let def = &CATALOG[index];
        let fits = matches!(
            (&update, def.kind),
            (Update::Add(_), Kind::Counter)
                | (Update::Set(_), Kind::Gauge)
                | (Update::Observe(_), Kind::Histogram)
        );
        let finite = match update {
            Update::Set(v) => v.is_finite(),
            Update::Observe(v) => v.is_finite() && v >= 0.0,
            Update::Add(_) => true,
        };
        if !fits || !finite {
            return false;
        }
        let Some(labels) = self.labels(def, given) else {
            return false;
        };
        let mut all = self.series.lock().unwrap_or_else(|e| e.into_inner());
        let series = &mut all[index];
        if !series.contains_key(&labels) && series.len() >= MAX_SERIES_PER_METRIC {
            return false;
        }
        let value = series.entry(labels).or_insert_with(|| match def.kind {
            Kind::Counter => Value::Counter(0),
            Kind::Gauge => Value::Gauge(0.0),
            Kind::Histogram => Value::Histogram {
                count: 0,
                sum: 0.0,
                buckets: vec![0; HISTOGRAM_BOUNDS_S.len() + 1],
            },
        });
        match (value, update) {
            (Value::Counter(total), Update::Add(d)) => *total = total.saturating_add(d),
            (Value::Gauge(g), Update::Set(v)) => *g = v,
            (
                Value::Histogram {
                    count,
                    sum,
                    buckets,
                },
                Update::Observe(v),
            ) => {
                *count = count.saturating_add(1);
                *sum += v;
                let bucket = HISTOGRAM_BOUNDS_S
                    .iter()
                    .position(|b| v <= *b)
                    .unwrap_or(HISTOGRAM_BOUNDS_S.len());
                buckets[bucket] = buckets[bucket].saturating_add(1);
            }
            _ => return false,
        }
        true
    }
}

impl Meter for Metrics {
    fn add(&self, name: &str, labels: &[(&str, &str)], delta: u64) {
        self.update(name, labels, Update::Add(delta));
    }

    fn set(&self, name: &str, labels: &[(&str, &str)], value: f64) {
        self.update(name, labels, Update::Set(value));
    }

    fn observe(&self, name: &str, labels: &[(&str, &str)], value: f64) {
        self.update(name, labels, Update::Observe(value));
    }
}
