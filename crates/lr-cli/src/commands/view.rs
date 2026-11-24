//! `status` and `brokers` (spec 9.1, 11.1): text for people, `--json` for agents. Every job of
//! the JSON carries its project, source and label (absent values are `null`).

use super::history_cmd::{reason_text, show_lines, shown_state};
use super::jobs::hub;
use super::log_cmd::cli_redactor;
use super::timefmt::{format_ms, format_time, Recent};
use super::SourceArg;
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_core::{JobId, Timestamp};
use lr_obs::Redactor;
use lr_proto::{
    BrokerStatus, CliRead, CliReply, CliRequest, JobDetail, JobSummary, ReadReply, ReadRequest,
    RecentFilter, RunnerInfo, RunnerLiveness, StatusFilter, StatusView, WireSource,
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
                    "last_output_ago_ms": r.last_output_ago_ms,
                }))).collect::<Vec<_>>(),
                "queue": v.queue.iter().map(|q| item(&q.summary, json!({
                    "position": q.position,
                    "wait_reason": q.wait_reason,
                    "priority": q.priority,
                }))).collect::<Vec<_>>(),
                "recent": v.recent.iter().map(|s| item(s, Value::Object(Map::new()))).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({ "brokers": brokers })
}

/// Masks the last lines of running jobs with `redactor` before anything prints them.
pub fn mask_status(views: &mut [(String, StatusView)], redactor: &Redactor) {
    for (_, v) in views.iter_mut() {
        for r in v.running.iter_mut() {
            for l in r.last_lines.iter_mut() {
                *l = redactor.redact_job_text(l);
            }
        }
    }
}

fn age_ms(s: &JobSummary, now: Timestamp) -> u64 {
    now.0.saturating_sub(s.queued_at.0)
}

/// Adds `age_s` (seconds since the job was queued, by `now` of this machine) to every job of
/// a `status --json` document.
pub fn add_ages(doc: &mut Value, now: Timestamp) {
    let Some(brokers) = doc["brokers"].as_array_mut() else {
        return;
    };
    for b in brokers {
        for list in ["running", "queue", "recent"] {
            let Some(jobs) = b[list].as_array_mut() else {
                continue;
            };
            for j in jobs {
                let queued = j["queued_at"].as_u64().unwrap_or(now.0);
                j["age_s"] = json!(now.0.saturating_sub(queued) / 1000);
            }
        }
    }
}

pub(crate) fn source_name(source: Option<WireSource>) -> &'static str {
    match source {
        Some(WireSource::Session) => "session",
        Some(WireSource::Github) => "github",
        None => "-",
    }
}

fn line(s: &JobSummary, tail: &str, now: Timestamp) -> String {
    let mut out = format!(
        "  {}  {:?}  {}  {}  {}  {}  {}{}",
        s.job,
        s.state,
        s.project.as_deref().unwrap_or("-"),
        source_name(s.source),
        s.label.as_deref().unwrap_or("-"),
        s.runner.as_deref().unwrap_or("-"),
        format_ms(age_ms(s, now)),
        tail
    );
    let mut details = Vec::new();
    if let Some(b) = &s.branch {
        details.push(format!("branch {b}"));
    }
    if let Some(d) = &s.cwd {
        details.push(format!("dir {d}"));
    }
    if let Some(n) = &s.note {
        details.push(format!("note: {n}"));
    }
    if !s.labels.is_empty() {
        details.push(format!("labels: {}", s.labels.join(", ")));
    }
    if !details.is_empty() {
        out.push_str(&format!("\n      {}", details.join("  ")));
    }
    out
}

/// The text of `status` at `now`: per broker the pool load, then running and queued jobs, each
/// with its age and, where known, branch, directory and note.
pub fn status_text(views: &[(String, StatusView)], now: Timestamp) -> String {
    let mut out = Vec::new();
    for (broker, v) in views {
        out.push(format!(
            "broker {broker}: memory {}/{} MB, cpu {}/{}",
            v.pool.used_mem_mb, v.pool.total_mem_mb, v.pool.used_cpu, v.pool.total_cpu
        ));
        for r in &v.running {
            let step = r
                .steps
                .iter()
                .rev()
                .find(|s| s.started_at.is_some())
                .map(|s| format!("  step {}", s.name))
                .unwrap_or_default();
            let quiet = r
                .last_output_ago_ms
                .map(|ms| format!("  last output {} ago", format_ms(ms)))
                .unwrap_or_default();
            out.push(line(&r.summary, &format!("{step}{quiet}"), now));
        }
        for q in &v.queue {
            let why = q
                .wait_reason
                .as_deref()
                .map(|w| format!("  #{} waits: {w}", q.position))
                .unwrap_or_else(|| format!("  #{}", q.position));
            let priority = q
                .priority
                .map(|p| format!("  priority {p}"))
                .unwrap_or_default();
            out.push(line(&q.summary, &format!("{why}{priority}"), now));
        }
        if v.running.is_empty() && v.queue.is_empty() {
            out.push("  (no jobs)".into());
        }
    }
    out.join("\n")
}

/// The `recent` block of `status --recent`: per broker the orders that left the queue, newest
/// end first, each with its final state, the reason and who ended it, and when.
pub fn recent_lines(views: &[(String, StatusView)]) -> Vec<String> {
    let mut out = Vec::new();
    for (broker, v) in views {
        out.push(format!("broker {broker}: recent"));
        for s in &v.recent {
            out.push(format!(
                "  {}  {}  {}  ended {}",
                s.job,
                shown_state(s),
                reason_text(s),
                s.finished_at.map_or("-".into(), format_time)
            ));
        }
        if v.recent.is_empty() {
            out.push("  (none)".into());
        }
    }
    out
}

/// The hub's known brokers.
pub async fn known_brokers(client: &IpcClient) -> Result<Vec<BrokerStatus>, String> {
    match client.call::<_, CliReply>(&CliRequest::Brokers).await? {
        CliReply::Brokers { brokers } => Ok(brokers),
        CliReply::Error(e) => Err(e.message),
        other => Err(format!("unexpected answer: {other:?}")),
    }
}

/// The line for a broker the hub is not connected to: the hub retries on its own, every 1 to
/// 30 s, with no limit.
pub fn unreachable_line(name: &str, since: Option<Timestamp>) -> String {
    match since {
        Some(t) => format!(
            "broker {name} unreachable since {} (the hub retries on its own)",
            super::timefmt::format_time(t)
        ),
        None => format!("broker {name} unreachable (the hub retries on its own)"),
    }
}

/// The reachable brokers to ask (only `broker` when given) and a line per broker that is not
/// reachable or not known.
pub async fn reachable(
    client: &IpcClient,
    broker: Option<&str>,
) -> Result<(Vec<String>, Vec<String>), String> {
    let mut names = Vec::new();
    let mut problems = Vec::new();
    for b in known_brokers(client).await? {
        if broker.is_some_and(|w| w != b.name) {
            continue;
        }
        if b.available {
            names.push(b.name);
        } else {
            problems.push(unreachable_line(&b.name, b.since));
        }
    }
    if let Some(w) = broker.filter(|_| names.is_empty() && problems.is_empty()) {
        problems.push(format!("broker {w} is not known to this hub"));
    }
    Ok((names, problems))
}

/// One line for an answer of `broker` that is not the data asked for.
pub fn problem(broker: &str, reply: &ReadReply) -> String {
    match reply {
        ReadReply::NotFound { what } => format!("broker {broker}: {what} not found"),
        ReadReply::Denied { reason } => format!("broker {broker}: refused: {reason}"),
        ReadReply::Failed { message } => format!("broker {broker}: {message}"),
        ReadReply::Unknown => format!("broker {broker}: the broker does not know this request"),
        other => format!("broker {broker}: unexpected answer {other:?}"),
    }
}

/// The runners of an answer to `Runners`; `None` for a broker that does not know the request.
pub fn runners_of(reply: ReadReply) -> Option<Vec<RunnerInfo>> {
    match reply {
        ReadReply::Runners { runners, .. } => Some(runners),
        _ => None,
    }
}

fn not_serving(r: &RunnerInfo) -> Option<&'static str> {
    match (r.liveness, r.online) {
        (Some(RunnerLiveness::Stale), _) => Some("stale"),
        (Some(RunnerLiveness::Offline), _) | (None, false) => Some("offline"),
        _ => None,
    }
}

/// Adds `runners` to every broker of a `status --json` document that answered `Runners`: the
/// runners that are `stale` or `offline`, with their last seen time and the silence in seconds.
pub fn add_runners(doc: &mut Value, per: &[(String, Vec<RunnerInfo>)]) {
    let Some(brokers) = doc["brokers"].as_array_mut() else {
        return;
    };
    for b in brokers {
        let Some((_, runners)) = per.iter().find(|(name, _)| b["broker"] == name.as_str()) else {
            continue;
        };
        let listed: Vec<Value> = runners
            .iter()
            .filter_map(|r| {
                not_serving(r).map(|state| {
                    json!({
                        "name": r.name,
                        "liveness": state,
                        "last_seen": r.last_seen,
                        "silent_for_s": r.silent_for_s,
                    })
                })
            })
            .collect();
        b["runners"] = json!(listed);
    }
}

/// The lines of `status` for runners that are `stale` or `offline`; empty when all serve.
pub fn not_serving_lines(per: &[(String, Vec<RunnerInfo>)]) -> Vec<String> {
    let mut out = Vec::new();
    for (broker, runners) in per {
        let bad: Vec<(&RunnerInfo, &str)> = runners
            .iter()
            .filter_map(|r| not_serving(r).map(|s| (r, s)))
            .collect();
        if bad.is_empty() {
            continue;
        }
        out.push(format!("broker {broker}: runners that do not take work"));
        for (r, state) in bad {
            let silent = r.silent_for_s.map_or(String::new(), |s| {
                format!(", silent {}", format_ms(s * 1000))
            });
            out.push(format!(
                "  {}  {state}  last seen {}{silent}",
                r.name,
                r.last_seen.map_or("-".into(), format_time)
            ));
        }
    }
    out
}

/// The lines of `status <job>`: the `explain` text and the record of `history show`, when the
/// broker gave one.
pub fn job_lines(broker: &str, job: &JobId, text: &str, detail: Option<&JobDetail>) -> Vec<String> {
    let mut out = vec![format!("{job} (broker {broker}): {text}")];
    if let Some(d) = detail {
        out.extend(show_lines(broker, d));
    }
    out
}

/// `status <job>`: the answer of the first broker that has the job.
pub async fn status_job(
    layout: &Layout,
    job: String,
    broker: Option<String>,
    run_client: Option<String>,
    json_out: bool,
) -> Result<i32, String> {
    let client = hub(layout).await?;
    let found = find_job(&client, &JobId(job.clone()), broker.as_deref(), run_client).await;
    client.close().await;
    let (b, text, detail) = found?;
    if json_out {
        print_json(&json!({ "broker": b, "job": job, "text": text, "detail": detail }))?;
    } else {
        for l in job_lines(&b, &JobId(job), &text, detail.as_ref()) {
            println!("{l}");
        }
    }
    Ok(0)
}

pub async fn find_job(
    client: &IpcClient,
    job: &JobId,
    broker: Option<&str>,
    run_client: Option<String>,
) -> Result<(String, String, Option<JobDetail>), String> {
    let (names, mut problems) = reachable(client, broker).await?;
    let mut ambiguous = Vec::new();
    for b in names {
        let ask = ReadRequest::Explain {
            job: job.clone(),
            client: run_client.clone(),
        };
        match read(client, &b, ask).await {
            Ok(ReadReply::Explain { text, .. }) => {
                let show = ReadRequest::HistoryShow {
                    job: job.clone(),
                    client: run_client.clone(),
                };
                let detail = match read(client, &b, show).await {
                    Ok(ReadReply::Job(d)) => Some(*d),
                    _ => None,
                };
                return Ok((b, text, detail));
            }
            Ok(ReadReply::NotFound { what }) if is_plain_missing(&what, job) => {}
            Ok(ReadReply::NotFound { what }) => ambiguous.push(format!("broker {b}: {what}")),
            Ok(other) => problems.push(problem(&b, &other)),
            Err(e) => problems.push(e),
        }
    }
    Err(missing_job_error(job, ambiguous, problems))
}

/// Whether what of a NotFound answer just says that job does not exist, as against the
/// broker's remark that the id is used by several clients.
pub fn is_plain_missing(what: &str, job: &JobId) -> bool {
    what == format!("job {job}")
}

/// The error for a job no broker answered: the brokers' remarks on an ambiguous id when there
/// are any (the job exists, --client picks one), else that it is not found, then the problems.
pub fn missing_job_error(job: &JobId, ambiguous: Vec<String>, problems: Vec<String>) -> String {
    let mut lines = if ambiguous.is_empty() {
        vec![format!(
            "job {job} is not found on the brokers of this hub: none of them recorded an order with this id (refused and cancelled orders are recorded too)"
        )]
    } else {
        ambiguous
    };
    lines.extend(problems);
    lines.join("\n")
}

/// Prints `value` as pretty JSON.
pub fn print_json(value: &impl serde::Serialize) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|e| e.to_string())?
    );
    Ok(())
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

/// Applies `recent` to the orders that left the queue on this side too: a broker older than
/// the filter ignores it and sends its latest ones, which are cut here to those that ended at
/// or after `since` and to at most `count`, newest first as they came.
pub fn clip_recent(view: &mut StatusView, recent: &RecentFilter) {
    if let Some(since) = recent.since {
        view.recent
            .retain(|s| s.finished_at.is_some_and(|end| end >= since));
    }
    if let Some(count) = recent.count {
        view.recent
            .truncate(usize::try_from(count).unwrap_or(usize::MAX));
    }
}

/// What `status` asks for.
#[derive(Debug, Clone, Default)]
pub struct StatusArgs {
    /// Only this project.
    pub project: Option<String>,
    /// Only this source.
    pub source: Option<SourceArg>,
    /// Only this label.
    pub label: Option<String>,
    /// Only this broker.
    pub broker: Option<String>,
    /// Also list the orders that left the queue.
    pub recent: Option<Recent>,
}

/// `status`.
pub async fn status(layout: &Layout, args: StatusArgs, json_out: bool) -> Result<i32, String> {
    let StatusArgs {
        project,
        source,
        label,
        broker,
        recent,
    } = args;
    let client = hub(layout).await?;
    let filter = StatusFilter {
        project,
        source: source.map(|s| match s {
            SourceArg::Session => WireSource::Session,
            SourceArg::Github => WireSource::Github,
        }),
        label,
        recent: recent.map(|r| r.filter(super::timefmt::now())),
        ..StatusFilter::default()
    };
    let mut views = Vec::new();
    let mut problems = Vec::new();
    for b in known_brokers(&client).await? {
        if broker.as_deref().is_some_and(|w| w != b.name) {
            continue;
        }
        if !b.available {
            problems.push(unreachable_line(&b.name, b.since));
            continue;
        }
        match read(&client, &b.name, ReadRequest::Status(filter.clone())).await {
            Ok(ReadReply::Status(v)) => {
                let mut v = *v;
                if let Some(r) = &filter.recent {
                    clip_recent(&mut v, r);
                }
                views.push((b.name, v))
            }
            Ok(other) => problems.push(format!("broker {}: {other:?}", b.name)),
            Err(e) => problems.push(e),
        }
    }
    let mut per_runners = Vec::new();
    for (name, _) in &views {
        if let Ok(reply) = read(&client, name, ReadRequest::Runners).await {
            if let Some(runners) = runners_of(reply) {
                per_runners.push((name.clone(), runners));
            }
        }
    }
    mask_status(&mut views, &cli_redactor(layout));
    let now = super::timefmt::now();
    let drift = crate::config_drift::lines(layout).await;
    if json_out {
        let mut out = status_json(&views);
        add_ages(&mut out, now);
        add_runners(&mut out, &per_runners);
        out["config_drift"] = json!(drift);
        if let Some(lock) = crate::upgrade_lock::live_here(layout) {
            out["upgrade"] = json!(crate::upgrade_lock::describe(&lock));
        }
        out["problems"] = json!(problems);
        println!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
        );
    } else {
        if let Some(lock) = crate::upgrade_lock::live_here(layout) {
            println!("{}", crate::upgrade_lock::describe(&lock));
        }
        let text = status_text(&views, now);
        if !text.is_empty() {
            println!("{text}");
        }
        if recent.is_some() {
            for line in recent_lines(&views) {
                println!("{line}");
            }
        }
        for line in not_serving_lines(&per_runners) {
            println!("{line}");
        }
        for line in &drift {
            println!("warn: {line}");
        }
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
