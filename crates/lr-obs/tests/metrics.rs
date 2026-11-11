use lr_abstractions::Meter;
use lr_core::Timestamp;
use lr_obs::metrics::{
    Kind, Metrics, Value, CATALOG, CLIENT_LABEL, CLIENT_LABEL_METRICS, HISTOGRAM_BOUNDS_S,
    MAX_LABEL_VALUE_BYTES, MAX_SERIES_PER_METRIC, PREFIX,
};

const T0: Timestamp = Timestamp(1_700_000_000_000);

fn metrics() -> Metrics {
    Metrics::new(T0, false)
}

fn only(m: &Metrics, name: &str) -> Value {
    let points: Vec<_> = m
        .snapshot()
        .into_iter()
        .filter(|p| p.name == name)
        .collect();
    assert_eq!(points.len(), 1, "{name}: {points:?}");
    points[0].value.clone()
}

#[test]
fn metrics_names_match_spec_12() {
    use Kind::*;
    let expected: &[(&str, Kind, &[&str], &[&str])] = &[
        ("queue_length", Gauge, &["platform"], &["broker"]),
        ("queue_wait_seconds", Histogram, &["platform"], &["broker"]),
        ("running_jobs", Gauge, &["pool", "runner"], &["broker"]),
        (
            "pool_memory_used_mb",
            Gauge,
            &["pool", "runner"],
            &["broker"],
        ),
        ("pool_cpu_used", Gauge, &["pool", "runner"], &["broker"]),
        ("saturation_wait_seconds", Gauge, &["reason"], &["broker"]),
        (
            "orders_total",
            Counter,
            &["result", "platform"],
            &["broker"],
        ),
        (
            "job_duration_seconds",
            Histogram,
            &["result", "platform"],
            &["broker"],
        ),
        ("runner_online", Gauge, &["runner"], &["broker"]),
        ("cache_used_mb", Gauge, &["runner"], &["runner"]),
        (
            "cache_evicted_total",
            Counter,
            &["runner", "reason"],
            &["runner"],
        ),
        ("service_up", Gauge, &["runner", "service"], &["runner"]),
        (
            "service_starts_total",
            Counter,
            &["runner", "service"],
            &["runner"],
        ),
        (
            "service_failures_total",
            Counter,
            &["runner", "service"],
            &["runner"],
        ),
        (
            "outbox_pending",
            Gauge,
            &["role"],
            &["hub", "broker", "runner"],
        ),
        (
            "outbox_oldest_age_seconds",
            Gauge,
            &["role"],
            &["hub", "broker", "runner"],
        ),
        ("policy_reload_total", Counter, &["result"], &["broker"]),
        (
            "log_dropped_total",
            Counter,
            &["sink"],
            &["hub", "broker", "runner"],
        ),
    ];
    assert_eq!(PREFIX, "localrunner_");
    assert_eq!(CATALOG.len(), expected.len());
    for (def, (short, kind, labels, roles)) in CATALOG.iter().zip(expected) {
        assert_eq!(def.name, format!("{PREFIX}{short}"));
        assert_eq!(def.kind, *kind, "{short}");
        assert_eq!(def.labels, *labels, "{short}");
        assert_eq!(def.roles, *roles, "{short}");
    }
}

#[test]
fn units_follow_metric_names() {
    let units: Vec<(&str, &str)> = CATALOG.iter().map(|d| (d.name, d.unit)).collect();
    assert_eq!(units.len(), 18);
    let expected = [
        "{order}",
        "s",
        "{job}",
        "MBy",
        "{cpu}",
        "s",
        "{order}",
        "s",
        "1",
        "MBy",
        "{entry}",
        "1",
        "{start}",
        "{failure}",
        "{message}",
        "s",
        "{reload}",
        "{event}",
    ];
    for ((name, unit), want) in units.iter().zip(expected) {
        assert_eq!(*unit, want, "{name}");
    }
}

#[test]
fn constrained_label_values_are_exact() {
    let values = |name: &str, label: &str| -> Option<Vec<&'static str>> {
        let def = CATALOG.iter().find(|d| d.name == name).unwrap();
        def.values
            .iter()
            .find(|(l, _)| *l == label)
            .map(|(_, v)| v.to_vec())
    };
    assert_eq!(
        values("localrunner_saturation_wait_seconds", "reason").unwrap(),
        vec!["runner_busy", "memory", "cpu", "tag", "rule"]
    );
    assert_eq!(
        values("localrunner_cache_evicted_total", "reason").unwrap(),
        vec!["size", "age"]
    );
    assert_eq!(
        values("localrunner_outbox_pending", "role").unwrap(),
        vec!["hub", "broker", "runner"]
    );
    assert_eq!(
        values("localrunner_log_dropped_total", "sink").unwrap(),
        vec!["file", "console", "journal", "otlp"]
    );
    assert_eq!(
        values("localrunner_policy_reload_total", "result").unwrap(),
        vec!["ok", "error"]
    );
    let results = vec![
        "passed",
        "failed",
        "timeout",
        "cancelled",
        "lost",
        "rejected",
    ];
    assert_eq!(
        values("localrunner_orders_total", "result").unwrap(),
        results
    );
    assert_eq!(
        values("localrunner_job_duration_seconds", "result").unwrap(),
        results
    );
    assert_eq!(values("localrunner_queue_length", "platform"), None);
}

#[test]
fn value_outside_the_allowed_set_is_rejected() {
    let m = metrics();
    m.set(
        "localrunner_saturation_wait_seconds",
        &[("reason", "other")],
        1.0,
    );
    m.add(
        "localrunner_cache_evicted_total",
        &[("runner", "r1"), ("reason", "age")],
        1,
    );
    assert_eq!(m.rejected(), 1);
    assert_eq!(m.snapshot().len(), 1);
}

#[test]
fn unknown_name_wrong_kind_and_wrong_labels_are_rejected() {
    let m = metrics();
    m.add("localrunner_jobs_total", &[], 1);
    m.add("queue_length", &[("platform", "p")], 1);
    m.set(
        "localrunner_orders_total",
        &[("result", "passed"), ("platform", "p")],
        1.0,
    );
    m.add("localrunner_orders_total", &[("result", "passed")], 1);
    m.add(
        "localrunner_orders_total",
        &[("result", "passed"), ("platform", "p"), ("project", "x")],
        1,
    );
    m.add(
        "localrunner_orders_total",
        &[("result", "passed"), ("result", "failed")],
        1,
    );
    assert_eq!(m.rejected(), 6);
    assert!(m.snapshot().is_empty());
}

#[test]
fn label_order_does_not_create_new_series() {
    let m = metrics();
    m.add(
        "localrunner_orders_total",
        &[("result", "passed"), ("platform", "p")],
        1,
    );
    m.add(
        "localrunner_orders_total",
        &[("platform", "p"), ("result", "passed")],
        2,
    );
    assert_eq!(only(&m, "localrunner_orders_total"), Value::Counter(3));
    let p = &m.snapshot()[0];
    assert_eq!(
        p.labels,
        vec![
            ("result".to_string(), "passed".to_string()),
            ("platform".to_string(), "p".to_string())
        ]
    );
}

#[test]
fn client_label_is_off_by_default_and_stripped() {
    assert_eq!(CLIENT_LABEL, "client");
    assert_eq!(
        CLIENT_LABEL_METRICS,
        &[
            "localrunner_queue_wait_seconds",
            "localrunner_orders_total",
            "localrunner_job_duration_seconds"
        ]
    );
    let m = metrics();
    m.add(
        "localrunner_orders_total",
        &[("result", "passed"), ("platform", "p"), ("client", "ci")],
        1,
    );
    assert_eq!(m.snapshot()[0].labels.len(), 2);
    assert_eq!(m.rejected(), 0);
}

#[test]
fn client_label_kept_when_enabled_only_where_allowed() {
    let m = Metrics::new(T0, true);
    m.add(
        "localrunner_orders_total",
        &[("result", "passed"), ("platform", "p"), ("client", "ci")],
        1,
    );
    m.add(
        "localrunner_orders_total",
        &[("result", "passed"), ("platform", "p")],
        1,
    );
    m.set(
        "localrunner_runner_online",
        &[("runner", "r"), ("client", "ci")],
        1.0,
    );
    assert_eq!(m.snapshot().len(), 2);
    assert_eq!(m.rejected(), 1);
    assert!(m
        .snapshot()
        .iter()
        .any(|p| p.labels.contains(&("client".to_string(), "ci".to_string()))));
}

#[test]
fn series_per_metric_are_capped() {
    assert_eq!(MAX_SERIES_PER_METRIC, 256);
    let m = metrics();
    for n in 0..=MAX_SERIES_PER_METRIC {
        m.set(
            "localrunner_runner_online",
            &[("runner", &format!("r{n}"))],
            1.0,
        );
    }
    assert_eq!(m.snapshot().len(), MAX_SERIES_PER_METRIC);
    assert_eq!(m.rejected(), 1);
    m.set("localrunner_runner_online", &[("runner", "r0")], 0.0);
    assert_eq!(m.rejected(), 1);
}

#[test]
fn label_values_are_bounded_and_printable() {
    assert_eq!(MAX_LABEL_VALUE_BYTES, 128);
    let m = metrics();
    let at_limit = "r".repeat(MAX_LABEL_VALUE_BYTES);
    let over = "r".repeat(MAX_LABEL_VALUE_BYTES + 1);
    m.set("localrunner_runner_online", &[("runner", &at_limit)], 1.0);
    m.set("localrunner_runner_online", &[("runner", &over)], 1.0);
    m.set("localrunner_runner_online", &[("runner", "a\nb")], 1.0);
    m.set("localrunner_runner_online", &[("runner", "")], 1.0);
    assert_eq!(m.snapshot().len(), 1);
    assert_eq!(m.rejected(), 3);
}

#[test]
fn counters_saturate() {
    let m = metrics();
    let labels = [("result", "ok")];
    m.add("localrunner_policy_reload_total", &labels, u64::MAX - 1);
    m.add("localrunner_policy_reload_total", &labels, 5);
    assert_eq!(
        only(&m, "localrunner_policy_reload_total"),
        Value::Counter(u64::MAX)
    );
}

#[test]
fn gauges_take_last_value_and_refuse_non_finite() {
    let m = metrics();
    let labels = [("role", "hub")];
    m.set("localrunner_outbox_pending", &labels, 3.0);
    m.set("localrunner_outbox_pending", &labels, 2.0);
    m.set("localrunner_outbox_pending", &labels, f64::NAN);
    m.set("localrunner_outbox_pending", &labels, f64::INFINITY);
    assert_eq!(only(&m, "localrunner_outbox_pending"), Value::Gauge(2.0));
    assert_eq!(m.rejected(), 2);
}

#[test]
fn histogram_bounds_are_exact() {
    assert_eq!(
        HISTOGRAM_BOUNDS_S,
        &[1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0, 7200.0, 14400.0]
    );
}

#[test]
fn histogram_counts_each_bucket_upper_bound_inclusive() {
    let m = metrics();
    let labels = [("platform", "p")];
    for v in [0.0, 1.0, 1.5, 14400.0, 14400.5] {
        m.observe("localrunner_queue_wait_seconds", &labels, v);
    }
    m.observe("localrunner_queue_wait_seconds", &labels, -1.0);
    m.observe("localrunner_queue_wait_seconds", &labels, f64::NAN);
    let mut buckets = vec![0u64; HISTOGRAM_BOUNDS_S.len() + 1];
    buckets[0] = 2;
    buckets[1] = 1;
    buckets[11] = 1;
    buckets[12] = 1;
    assert_eq!(
        only(&m, "localrunner_queue_wait_seconds"),
        Value::Histogram {
            count: 5,
            sum: 28803.0,
            buckets
        }
    );
    assert_eq!(m.rejected(), 2);
}

#[test]
fn snapshot_is_sorted_and_starts_at_creation_time() {
    let m = metrics();
    m.set("localrunner_runner_online", &[("runner", "b")], 1.0);
    m.set("localrunner_runner_online", &[("runner", "a")], 1.0);
    m.set("localrunner_queue_length", &[("platform", "p")], 1.0);
    let names: Vec<(String, String)> = m
        .snapshot()
        .iter()
        .map(|p| (p.name.to_string(), p.labels[0].1.clone()))
        .collect();
    assert_eq!(
        names,
        vec![
            ("localrunner_queue_length".into(), "p".into()),
            ("localrunner_runner_online".into(), "a".into()),
            ("localrunner_runner_online".into(), "b".into()),
        ]
    );
    assert_eq!(m.start(), T0);
}

#[test]
fn forgetting_a_series_keeps_the_same_name_of_another_runner() {
    let m = metrics();
    for runner in ["r1", "r2"] {
        m.set(
            "localrunner_service_up",
            &[("runner", runner), ("service", "db")],
            1.0,
        );
    }
    m.set(
        "localrunner_service_up",
        &[("runner", "r1"), ("service", "web")],
        1.0,
    );
    m.forget_series(&[("runner", "r1"), ("service", "db")]);
    let left: Vec<Vec<(String, String)>> = m.snapshot().into_iter().map(|p| p.labels).collect();
    let pair = |r: &str, s: &str| {
        vec![
            ("runner".to_string(), r.to_string()),
            ("service".to_string(), s.to_string()),
        ]
    };
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(left.contains(&pair("r2", "db")) && left.contains(&pair("r1", "web")));
}

#[test]
fn forgetting_an_entity_removes_its_series() {
    let m = metrics();
    m.set("localrunner_runner_online", &[("runner", "r1")], 1.0);
    m.set("localrunner_runner_online", &[("runner", "r2")], 1.0);
    m.set(
        "localrunner_service_up",
        &[("runner", "r1"), ("service", "db")],
        1.0,
    );
    m.add(
        "localrunner_service_starts_total",
        &[("runner", "r1"), ("service", "db")],
        1,
    );
    m.forget("runner", "r1");
    let left: Vec<(String, Vec<(String, String)>)> = m
        .snapshot()
        .into_iter()
        .map(|p| (p.name.to_string(), p.labels))
        .collect();
    assert_eq!(
        left,
        vec![(
            "localrunner_runner_online".to_string(),
            vec![("runner".to_string(), "r2".to_string())]
        )]
    );
    for n in 0..MAX_SERIES_PER_METRIC {
        m.set(
            "localrunner_runner_online",
            &[("runner", &format!("x{n}"))],
            1.0,
        );
        m.forget("runner", &format!("x{n}"));
    }
    m.set("localrunner_runner_online", &[("runner", "fresh")], 1.0);
    assert_eq!(m.rejected(), 0);
}
