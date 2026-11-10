//! `status` and `brokers` (spec 9.1, 11.1): text for people, `--json` for agents. Every job of
//! the JSON carries its project, source and label (absent values are `null`).

use super::jobs::hub;
use super::SourceArg;
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_proto::{
    BrokerStatus, CliRead, CliReply, CliRequest, JobSummary, ReadReply, ReadRequest, StatusFilter,
    StatusView, WireSource,
};
use serde_json::{json, Map, Value};

fn item(summary: &JobSummary, extra: Value) -> Value {
    let mut v = serde_json::to_value(summary).unwrap_or(Value::Null);
    if let (Value::Object(o), Value::Object(e)) = (&mut v, extra) {
        o.extend(e);
    }
    v
}

/// The JSON of `status --json`: per broker its pool load, running jobs (with steps and last
/// lines), the queue (with position and wait reason), recent jobs and the next order.
pub fn status_json(views: &[(String, StatusView)]) -> Value {
    let brokers: Vec<Value> = views
        .iter()
        .map(|(broker, v)| {
            json!({
                "broker": broker,
                "pool": v.pool,
                "next": v.next,
                "running": v.running.iter().map(|r| item(&r.summary, json!({
                    "steps": r.steps,
                    "last_lines": r.last_lines,
                }))).collect::<Vec<_>>(),
                "queue": v.queue.iter().map(|q| item(&q.summary, json!({
                    "position": q.position,
                    "wait_reason": q.wait_reason,
                }))).collect::<Vec<_>>(),
                "recent": v.recent.iter().map(|s| item(s, Value::Object(Map::new()))).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({ "brokers": brokers })
}

fn line(s: &JobSummary, tail: &str) -> String {
    format!(
        "  {}  {:?}  {}  {}  {}{}",
        s.job,
        s.state,
        s.project.as_deref().unwrap_or("-"),
        s.label.as_deref().unwrap_or("-"),
        s.runner.as_deref().unwrap_or("-"),
        tail
    )
}

fn print_text(views: &[(String, StatusView)]) {
    for (broker, v) in views {
        println!(
            "broker {broker}: memory {}/{} MB, cpu {}/{}",
            v.pool.used_mem_mb, v.pool.total_mem_mb, v.pool.used_cpu, v.pool.total_cpu
        );
        for r in &v.running {
            let step = r
                .steps
                .iter()
                .rev()
                .find(|s| s.started_at.is_some())
                .map(|s| format!("  step {}", s.name))
                .unwrap_or_default();
            println!("{}", line(&r.summary, &step));
        }
        for q in &v.queue {
            let why = q
                .wait_reason
                .as_deref()
                .map(|w| format!("  #{} waits: {w}", q.position))
                .unwrap_or_else(|| format!("  #{}", q.position));
            println!("{}", line(&q.summary, &why));
        }
        if v.running.is_empty() && v.queue.is_empty() {
            println!("  (no jobs)");
        }
    }
}

/// The hub's known brokers.
pub async fn known_brokers(client: &IpcClient) -> Result<Vec<BrokerStatus>, String> {
    match client.call::<_, CliReply>(&CliRequest::Brokers).await? {
        CliReply::Brokers { brokers } => Ok(brokers),
        CliReply::Error(e) => Err(e.message),
        other => Err(format!("unexpected answer: {other:?}")),
    }
}

/// Asks one broker through the hub.
pub async fn read(
    client: &IpcClient,
    broker: &str,
    request: ReadRequest,
) -> Result<ReadReply, String> {
    let req = CliRequest::Read(CliRead {
        broker: Some(broker.to_string()),
        request,
    });
    match client.call::<_, CliReply>(&req).await? {
        CliReply::Read { reply, .. } => Ok(reply),
        CliReply::Error(e) => Err(format!("broker {broker}: {}", e.message)),
        other => Err(format!("unexpected answer: {other:?}")),
    }
}

/// `status`.
pub async fn status(
    layout: &Layout,
    project: Option<String>,
    source: Option<SourceArg>,
    label: Option<String>,
    broker: Option<String>,
    json_out: bool,
) -> Result<i32, String> {
    let client = hub(layout).await?;
    let filter = StatusFilter {
        project,
        source: source.map(|s| match s {
            SourceArg::Session => WireSource::Session,
            SourceArg::Github => WireSource::Github,
        }),
        label,
        ..StatusFilter::default()
    };
    let mut views = Vec::new();
    let mut problems = Vec::new();
    for b in known_brokers(&client).await? {
        if broker.as_deref().is_some_and(|w| w != b.name) {
            continue;
        }
        if !b.available {
            problems.push(format!("broker {} is unreachable", b.name));
            continue;
        }
        match read(&client, &b.name, ReadRequest::Status(filter.clone())).await {
            Ok(ReadReply::Status(v)) => views.push((b.name, v)),
            Ok(other) => problems.push(format!("broker {}: {other:?}", b.name)),
            Err(e) => problems.push(e),
        }
    }
    if json_out {
        let mut out = status_json(&views);
        out["problems"] = json!(problems);
        println!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
        );
    } else {
        print_text(&views);
        for p in &problems {
            eprintln!("{p}");
        }
    }
    Ok(0)
}

/// `brokers`.
pub async fn brokers(layout: &Layout, json_out: bool) -> Result<i32, String> {
    let client = hub(layout).await?;
    let list = known_brokers(&client).await?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&list).map_err(|e| e.to_string())?
        );
    } else {
        for b in &list {
            println!(
                "{}\t{}\t{}\toutbox {}",
                b.name,
                b.zone
                    .map_or("-".to_string(), |z| format!("{z:?}").to_lowercase()),
                if b.available {
                    "available"
                } else {
                    "unreachable"
                },
                b.outbox
            );
        }
    }
    Ok(0)
}
