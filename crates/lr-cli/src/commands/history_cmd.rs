//! `history [filters]` and `history show <job>` (spec 8, 11.1): runs come from the brokers
//! through the hub, most recently finished first, by default only this client's. What a broker's visibility
//! policy hides arrives empty and is printed as `-`.

use super::jobs::hub;
use super::timefmt::{format_ms, format_time, now};
use super::view::{print_json, problem, reachable, read, source_name};
use super::{HistoryFilters, ResultArg, ShowArgs, SourceArg};
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_core::{JobId, JobState, Timestamp};
use lr_proto::{
    HistoryFilter, JobDetail, JobSummary, ReadReply, ReadRequest, ReasonCode, WireSource,
};
use serde_json::{json, Value};

/// The broker's filter from the flags: without `--client` and `--all` only this client's runs.
pub fn history_filter(f: &HistoryFilters, now: Timestamp) -> HistoryFilter {
    HistoryFilter {
        client: f.client.clone(),
        project: f.project.clone(),
        source: f.source.map(|s| match s {
            SourceArg::Session => WireSource::Session,
            SourceArg::Github => WireSource::Github,
        }),
        label: f.label.clone(),
        profile: f.profile.clone(),
        state: f.result.map(|r| r.state()),
        platform: f.platform.clone(),
        runner: f.runner.clone(),
        since: f.since.map(|s| s.resolve(now)),
        ran_since: f.ran_since.map(|s| s.resolve(now)),
        ran_until: f.ran_until.map(|s| s.resolve(now)),
        active_only: false,
        limit: f.limit,
        cursor: None,
        offset: None,
        mine: f.client.is_none() && !f.all,
    }
}

/// Whether the run was refused at intake and never started.
pub fn is_rejected(s: &JobSummary) -> bool {
    s.state == JobState::Cancelled && matches!(s.reason, Some(ReasonCode::Rejected(_)))
}

/// The state as `history` shows it: a refused order is `rejected`, not a plain cancellation.
pub fn shown_state(s: &JobSummary) -> String {
    if is_rejected(s) {
        "rejected".into()
    } else {
        state_name(s.state)
    }
}

/// Why the run ended as `history` and `status --recent` show it: `cancelled by <actor>: <code>`
/// for a cancelled run whose canceller is known, the reason code otherwise (a refusal is
/// already `rejected` in the state), `-` without one.
pub fn reason_text(s: &JobSummary) -> String {
    let by = s.cancelled_by.as_ref().filter(|b| !b.is_empty());
    match (&s.reason, by) {
        (Some(code), Some(by)) if s.state == JobState::Cancelled && !is_rejected(s) => {
            format!("cancelled by {by}: {code}")
        }
        (Some(code), _) => code.to_string(),
        (None, Some(by)) if s.state == JobState::Cancelled => format!("cancelled by {by}"),
        (None, _) => "-".into(),
    }
}

/// Whether the run was running at some moment of the window: it started at or before `until`
/// and did not finish before `since`; a run that is still going overlaps, one that never
/// started does not. A missing end leaves that side open.
pub fn overlaps(s: &JobSummary, since: Option<Timestamp>, until: Option<Timestamp>) -> bool {
    if since.is_none() && until.is_none() {
        return true;
    }
    let Some(started) = s.started_at else {
        return false;
    };
    until.is_none_or(|u| started <= u) && since.is_none_or(|t| s.finished_at.is_none_or(|f| f >= t))
}

/// The runs of every broker as one list, the most recently finished first (ties by job id,
/// runs without a finish time last in the order they came), at most `limit`.
pub fn newest_first(
    per: Vec<(String, Vec<JobSummary>)>,
    limit: usize,
) -> Vec<(String, JobSummary)> {
    let mut rows: Vec<(String, JobSummary)> = per
        .into_iter()
        .flat_map(|(b, jobs)| jobs.into_iter().map(move |j| (b.clone(), j)))
        .collect();
    rows.sort_by(|a, b| match (a.1.finished_at, b.1.finished_at) {
        (Some(x), Some(y)) => (y, &b.1.job).cmp(&(x, &a.1.job)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    rows.truncate(limit);
    rows
}

fn dash(v: &Option<String>) -> &str {
    v.as_deref().unwrap_or("-")
}

/// The state as the protocol names it (`passed`, `timeout`).
pub fn state_name(s: JobState) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn span(from: Option<Timestamp>, to: Option<Timestamp>) -> String {
    match (from, to) {
        (Some(a), Some(b)) => format_ms(b.0.saturating_sub(a.0)),
        _ => "-".into(),
    }
}

/// A header and one tab-separated line per run.
pub fn history_lines(rows: &[(String, JobSummary)]) -> Vec<String> {
    let mut out = vec![
        "JOB\tSTATE\tQUEUED\tSTARTED\tFINISHED\tDURATION\tPROFILE\tPROJECT\tSOURCE\tCLIENT_LABEL\tCLIENT\tRUNNER\tPEAK_MEM_MB\tPEAK_DISK_MB\tBROKER\tREASON"
            .into(),
    ];
    for (b, s) in rows {
        out.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{b}\t{}",
            s.job,
            shown_state(s),
            format_time(s.queued_at),
            when(s.started_at),
            when(s.finished_at),
            span(s.started_at, s.finished_at),
            dash(&s.profile),
            dash(&s.project),
            source_name(s.source),
            dash(&s.label),
            if s.client_label.is_empty() {
                "-"
            } else {
                &s.client_label
            },
            dash(&s.runner),
            mb(s.peak_mem_mb),
            mb(s.peak_disk_mb),
            reason_text(s),
        ));
    }
    out
}

fn labels_text(labels: &[String]) -> String {
    if labels.is_empty() {
        "-".into()
    } else {
        labels.join(", ")
    }
}

fn mb(v: Option<u64>) -> String {
    v.map_or("-".into(), |v| v.to_string())
}

/// A time as the list commands print it, `-` when unknown.
pub fn when(t: Option<Timestamp>) -> String {
    t.map_or("-".into(), format_time)
}

/// The card of `history show`.
pub fn show_lines(broker: &str, d: &JobDetail) -> Vec<String> {
    let s = &d.summary;
    let source = source_name(d.source.or(s.source));
    let exit = s.exit_code.map_or("-".into(), |c| c.to_string());
    let reason = s
        .reason
        .as_ref()
        .map_or(String::new(), |r| format!(", {r}"));
    let mut out = vec![
        format!("job       {} (broker {broker})", s.job),
        format!("state     {}, exit {exit}{reason}", shown_state(s)),
        format!("project   {}", dash(&s.project)),
        format!("client_label {}", dash(&s.label)),
        format!("client    {}", s.client_label),
        format!("profile   {}", dash(&s.profile)),
        format!("labels    {}", labels_text(&s.labels)),
        format!("source    {source}"),
        format!("note      {}", dash(&d.note)),
        format!("branch    {}", dash(&d.branch)),
        format!("directory {}", dash(&d.cwd)),
        format!("os user   {}", dash(&d.os_user)),
        format!("platform  {}", dash(&s.platform)),
        format!("runner    {}", dash(&s.runner)),
        format!("queued    {}", format_time(s.queued_at)),
        format!("started   {}", when(s.started_at)),
        format!("finished  {}", when(s.finished_at)),
    ];
    if let Some(m) = s.peak_mem_mb {
        out.push(format!("peak mem  {m} MB"));
    }
    if let Some(m) = s.peak_disk_mb {
        out.push(format!("peak disk {m} MB"));
    }
    let optional = [
        ("github    ", &d.github_run_url),
        (
            "cancelled ",
            &d.cancelled_by.clone().filter(|b| !b.is_empty()),
        ),
        ("waits     ", &d.wait_reason),
    ];
    for (name, value) in optional {
        if let Some(v) = value {
            out.push(format!("{name}{v}"));
        }
    }
    if !d.secrets.is_empty() {
        out.push(format!("secrets   {}", d.secrets.join(", ")));
    }
    if !d.steps.is_empty() {
        out.push("steps".into());
    }
    for st in &d.steps {
        let state = serde_json::to_value(st.state)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        out.push(format!(
            "  {} {}\t{state}\texit {}\t{}\t(timeout {}s)",
            st.ord,
            st.name,
            st.exit_code.map_or("-".into(), |c| c.to_string()),
            span(st.started_at, st.finished_at),
            st.timeout_s
        ));
    }
    out
}

/// Runs per reachable broker (only `broker` when given) and a line per broker that failed.
pub async fn fetch_history(
    client: &IpcClient,
    broker: Option<&str>,
    filter: &HistoryFilter,
) -> Result<(Vec<(String, Vec<JobSummary>)>, Vec<String>), String> {
    let (names, mut problems) = reachable(client, broker).await?;
    let mut per = Vec::new();
    for b in names {
        match read(client, &b, ReadRequest::History(filter.clone())).await {
            Ok(ReadReply::History { jobs, .. }) => per.push((b, jobs)),
            Ok(other) => problems.push(problem(&b, &other)),
            Err(e) => problems.push(e),
        }
    }
    Ok((per, problems))
}

/// `history [filters]`.
pub async fn history(layout: &Layout, f: HistoryFilters) -> Result<i32, String> {
    let client = hub(layout).await?;
    let filter = history_filter(&f, now());
    let (per, problems) = fetch_history(&client, f.broker.as_deref(), &filter).await?;
    client.close().await;
    let per = if f.result == Some(ResultArg::Rejected) {
        per.into_iter()
            .map(|(b, jobs)| (b, jobs.into_iter().filter(is_rejected).collect()))
            .collect()
    } else {
        per
    };
    let per = if filter.ran_since.is_some() || filter.ran_until.is_some() {
        per.into_iter()
            .map(|(b, jobs)| {
                let kept = jobs
                    .into_iter()
                    .filter(|s| overlaps(s, filter.ran_since, filter.ran_until))
                    .collect();
                (b, kept)
            })
            .collect()
    } else {
        per
    };
    let rows = newest_first(per, filter.effective_limit() as usize);
    if f.json {
        let jobs: Vec<Value> = rows
            .iter()
            .map(|(b, s)| {
                let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
                v["broker"] = json!(b);
                v
            })
            .collect();
        print_json(&json!({ "jobs": jobs, "problems": problems }))?;
    } else {
        for line in history_lines(&rows) {
            println!("{line}");
        }
        for p in &problems {
            eprintln!("{p}");
        }
    }
    Ok(0)
}

/// The full record of `job` from the first broker that has it.
pub async fn find_job(
    client: &IpcClient,
    broker: Option<&str>,
    job: &JobId,
) -> Result<(String, JobDetail), String> {
    let (names, mut problems) = reachable(client, broker).await?;
    for b in names {
        let req = ReadRequest::HistoryShow {
            job: job.clone(),
            client: None,
        };
        match read(client, &b, req).await {
            Ok(ReadReply::Job(d)) => return Ok((b, *d)),
            Ok(ReadReply::NotFound { .. }) => {}
            Ok(other) => problems.push(problem(&b, &other)),
            Err(e) => problems.push(e),
        }
    }
    problems.insert(
        0,
        format!("job {job} is not found on the brokers of this hub"),
    );
    Err(problems.join("\n"))
}

/// `history show <job>`.
pub async fn show(layout: &Layout, a: ShowArgs) -> Result<i32, String> {
    let client = hub(layout).await?;
    let found = find_job(&client, a.broker.as_deref(), &JobId(a.job)).await;
    client.close().await;
    let (broker, detail) = found?;
    if a.json {
        print_json(&json!({ "broker": broker, "job": detail }))?;
    } else {
        for line in show_lines(&broker, &detail) {
            println!("{line}");
        }
    }
    Ok(0)
}
