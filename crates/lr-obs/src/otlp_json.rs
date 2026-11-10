//! OTLP/HTTP JSON encoding of logs and metrics (64-bit integers as strings, as the OTLP JSON
//! mapping requires).

use crate::format::{level_name, role_name};
use crate::logger::Resource;
use crate::metrics::{Kind, Metrics, Point, Value, HISTOGRAM_BOUNDS_S};
use crate::otlp::severity_number;
use crate::redact::Redacted;
use lr_core::Timestamp;
use serde_json::{json, Value as Json};

const SCOPE: &str = "localrunner";
const CUMULATIVE: u8 = 2;

fn nanos(t: Timestamp) -> String {
    (u128::from(t.0) * 1_000_000).to_string()
}

fn kv(key: &str, value: &str) -> Json {
    json!({"key": key, "value": {"stringValue": value}})
}

fn resource(r: &Resource) -> Json {
    json!({"attributes": [
        kv("service.name", "localrunner"),
        kv("localrunner.role", role_name(r.role)),
        kv("localrunner.machine_id", &r.machine_id),
    ]})
}

fn log_record(r: &Redacted) -> Json {
    let rec = r.record();
    let mut attrs = vec![kv("component", &rec.component)];
    if !rec.machine_id.is_empty() {
        attrs.push(kv("machine_id", &rec.machine_id));
    }
    for (i, (name, value)) in rec.fields.iter().enumerate() {
        if !rec.fields[i + 1..].iter().any(|(n, _)| n == name) {
            attrs.push(kv(name, value));
        }
    }
    json!({
        "timeUnixNano": nanos(rec.time),
        "severityNumber": severity_number(rec.level),
        "severityText": level_name(rec.level).to_uppercase(),
        "body": {"stringValue": rec.message},
        "attributes": attrs,
    })
}

/// An `ExportLogsServiceRequest` with `batch`.
pub fn encode_logs(r: &Resource, batch: &[Redacted]) -> Vec<u8> {
    let records: Vec<Json> = batch.iter().map(log_record).collect();
    json!({"resourceLogs": [{
        "resource": resource(r),
        "scopeLogs": [{"scope": {"name": SCOPE}, "logRecords": records}],
    }]})
    .to_string()
    .into_bytes()
}

fn data_point(p: &Point, start: Timestamp, now: Timestamp) -> Json {
    let attrs: Vec<Json> = p.labels.iter().map(|(k, v)| kv(k, v)).collect();
    match &p.value {
        Value::Counter(total) => json!({
            "attributes": attrs,
            "startTimeUnixNano": nanos(start),
            "timeUnixNano": nanos(now),
            "asInt": total.to_string(),
        }),
        Value::Gauge(v) => json!({"attributes": attrs, "timeUnixNano": nanos(now), "asDouble": v}),
        Value::Histogram {
            count,
            sum,
            buckets,
        } => json!({
            "attributes": attrs,
            "startTimeUnixNano": nanos(start),
            "timeUnixNano": nanos(now),
            "count": count.to_string(),
            "sum": sum,
            "bucketCounts": buckets.iter().map(u64::to_string).collect::<Vec<_>>(),
            "explicitBounds": HISTOGRAM_BOUNDS_S,
        }),
    }
}

/// An `ExportMetricsServiceRequest` with every series of `metrics` at `now`.
pub fn encode_metrics(r: &Resource, metrics: &Metrics, now: Timestamp) -> Vec<u8> {
    let start = metrics.start();
    let mut out: Vec<Json> = Vec::new();
    let points = metrics.snapshot();
    let mut i = 0;
    while i < points.len() {
        let first = &points[i];
        let group: Vec<Json> = points[i..]
            .iter()
            .take_while(|p| p.name == first.name)
            .map(|p| data_point(p, start, now))
            .collect();
        i += group.len();
        let data = match first.kind {
            Kind::Counter => json!({"sum": {
                "aggregationTemporality": CUMULATIVE,
                "isMonotonic": true,
                "dataPoints": group,
            }}),
            Kind::Gauge => json!({"gauge": {"dataPoints": group}}),
            Kind::Histogram => json!({"histogram": {
                "aggregationTemporality": CUMULATIVE,
                "dataPoints": group,
            }}),
        };
        let mut metric = json!({"name": first.name, "unit": first.unit});
        if let (Some(m), Some(d)) = (metric.as_object_mut(), data.as_object()) {
            m.extend(d.clone());
        }
        out.push(metric);
    }
    json!({"resourceMetrics": [{
        "resource": resource(r),
        "scopeMetrics": [{"scope": {"name": SCOPE}, "metrics": out}],
    }]})
    .to_string()
    .into_bytes()
}
