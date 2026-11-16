//! `explain`, `stats` and `runners` (spec 8, 9.1, 11.1): answers of the brokers through the hub,
//! as text or `--json`.

use super::jobs::hub;
use super::timefmt::{format_ms, format_time, now};
use super::view::{is_plain_missing, missing_job_error, print_json, problem, reachable, read};
use super::{ExplainArgs, RunnersArgs, StatsArgs};
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_core::JobId;
use lr_proto::{
    PoolInfo, ReadReply, ReadRequest, RunnerInfo, RunnerKind, RunnerLiveness, SpreadView,
    StatsFilter, StatsView,
};
use serde_json::json;

/// A broker with its runners and the pools they draw on.
pub type RunnerView = (String, Vec<RunnerInfo>, Vec<PoolInfo>);

/// Why `job` waits, from the first broker that has it: the broker's name and its text, which
/// names the policy rule that holds the order.
pub async fn explain(
    client: &IpcClient,
    job: &JobId,
    broker: Option<&str>,
) -> Result<(String, String), String> {
    explain_for(client, job, broker, None).await
}

/// Like `explain`, for the run of the client key `run_client` when several clients use `job`.
pub async fn explain_for(
    client: &IpcClient,
    job: &JobId,
    broker: Option<&str>,
    run_client: Option<String>,
) -> Result<(String, String), String> {
    let (names, mut problems) = reachable(client, broker).await?;
    let mut ambiguous = Vec::new();
    for b in names {
        let req = ReadRequest::Explain {
            job: job.clone(),
            client: run_client.clone(),
        };
        match read(client, &b, req).await {
            Ok(ReadReply::Explain { text, .. }) => return Ok((b, text)),
            Ok(ReadReply::NotFound { what }) if is_plain_missing(&what, job) => {}
            Ok(ReadReply::NotFound { what }) => ambiguous.push(format!("broker {b}: {what}")),
            Ok(other) => problems.push(problem(&b, &other)),
            Err(e) => problems.push(e),
        }
    }
    Err(missing_job_error(job, ambiguous, problems))
}

/// `explain <job>`.
pub async fn explain_cmd(layout: &Layout, a: ExplainArgs) -> Result<i32, String> {
    let client = hub(layout).await?;
    let job = JobId(a.job);
    let found = explain_for(&client, &job, a.broker.as_deref(), a.client).await;
    client.close().await;
    let (broker, text) = found?;
    if a.json {
        print_json(&json!({ "broker": broker, "job": job, "text": text }))?;
    } else {
        println!("{job} (broker {broker}): {text}");
    }
    Ok(0)
}

fn spread(s: &SpreadView) -> String {
    let f = |v: Option<u64>| v.map_or("-".into(), format_ms);
    format!("median {} p95 {}", f(s.median), f(s.p95))
}

fn share(part: u64, all: u64) -> String {
    match all {
        0 => "-".into(),
        _ => format!("{}%", part * 100 / all),
    }
}

/// The statistics of every broker as text.
pub fn stats_lines(per: &[(String, StatsView)]) -> Vec<String> {
    let mut out = Vec::new();
    for (b, v) in per {
        out.push(format!("broker {b}"));
        if v.profiles.is_empty() && v.clients.is_empty() {
            out.push("  (no finished runs)".into());
        }
        for p in &v.profiles {
            out.push(format!(
                "  profile {}: {} runs, {} passed, {} failed, {} timeouts, {} cancelled, {} lost; \
                 wait {}; duration {}",
                p.profile,
                p.runs,
                share(p.passed, p.runs),
                p.failed,
                p.timeouts,
                p.cancelled,
                p.lost,
                spread(&p.wait_ms),
                spread(&p.duration_ms)
            ));
            for s in &p.steps {
                out.push(format!(
                    "    step {}: {} runs, {} passed; duration {}",
                    s.name,
                    s.runs,
                    share(s.passed, s.runs),
                    spread(&s.duration_ms)
                ));
            }
        }
        for c in &v.clients {
            out.push(format!(
                "  client {} ({}): {} runs, busy {}",
                c.label,
                c.client,
                c.runs,
                format_ms(c.busy_ms)
            ));
        }
    }
    out
}

/// `stats`.
pub async fn stats(layout: &Layout, a: StatsArgs) -> Result<i32, String> {
    let client = hub(layout).await?;
    let filter = StatsFilter {
        client: a.client,
        project: a.project,
        profile: a.profile,
        since: a.since.map(|s| s.resolve(now())),
    };
    let (names, mut problems) = reachable(&client, a.broker.as_deref()).await?;
    let mut per = Vec::new();
    for b in names {
        match read(&client, &b, ReadRequest::Stats(filter.clone())).await {
            Ok(ReadReply::Stats(v)) => per.push((b, v)),
            Ok(other) => problems.push(problem(&b, &other)),
            Err(e) => problems.push(e),
        }
    }
    client.close().await;
    if a.json {
        let brokers: Vec<_> = per
            .iter()
            .map(|(b, v)| json!({ "broker": b, "stats": v }))
            .collect();
        print_json(&json!({ "brokers": brokers, "problems": problems }))?;
    } else {
        for line in stats_lines(&per) {
            println!("{line}");
        }
        for p in &problems {
            eprintln!("{p}");
        }
    }
    Ok(0)
}

fn listed<'a>(items: impl Iterator<Item = &'a String>) -> String {
    let all: Vec<&str> = items.map(String::as_str).collect();
    if all.is_empty() {
        "-".into()
    } else {
        all.join(",")
    }
}

fn liveness_text(r: &RunnerInfo) -> String {
    match (r.liveness, r.online) {
        (Some(RunnerLiveness::Stale), _) => match r.silent_for_s {
            Some(s) => format!("stale (silent {s} s)"),
            None => "stale".into(),
        },
        (Some(RunnerLiveness::Online), _) | (None, true) => "online".into(),
        _ => "offline".into(),
    }
}

fn kind_column(r: &RunnerInfo) -> String {
    match r.kind {
        Some(RunnerKind::Local) => "\tlocal".into(),
        Some(RunnerKind::Joined) => "\tjoined".into(),
        Some(RunnerKind::Unknown) => "\t?".into(),
        None => String::new(),
    }
}

fn runner_block(r: &RunnerInfo, out: &mut Vec<String>) {
    let c = &r.caps;
    out.push(format!(
        "  {}\t{}{}\tlast seen {}\tplatforms {}\tmemory {} MB, free in the pool {} MB\tcpu {}, free in the pool {}\t\
         tags {}\tprograms {}",
        r.name,
        liveness_text(r),
        kind_column(r),
        r.last_seen.map_or("-".into(), format_time),
        listed(c.platforms.iter()),
        c.mem_mb,
        r.free_mem_mb,
        c.cpu,
        r.free_cpu,
        listed(c.tags.iter()),
        listed(c.programs.iter()),
    ));
    if let (Some(busy), Some(can)) = (r.busy, r.can_start_now) {
        out.push(format!(
            "    {}; can start a job now: {}",
            if busy { "busy" } else { "idle" },
            if can { "yes" } else { "no" }
        ));
    }
    let mine = env!("CARGO_PKG_VERSION");
    match c.version.as_deref() {
        Some(v) if v == mine => out.push(format!("    version {v}")),
        Some(v) => out.push(format!("    version {v} (this binary: {mine})")),
        None => out.push(format!("    version unknown (this binary: {mine})")),
    }
    if let Some(e) = &c.enforcement {
        let older = if c.enforce_limit {
            ""
        } else {
            " (limit mode not supported: runs as warn)"
        };
        out.push(format!("    enforcement {e}{older}"));
    }
    for t in &r.held_tags {
        let kind = if t.exclusive { ", exclusive" } else { "" };
        out.push(format!("    tag {} held by {}{kind}", t.tag, t.job));
    }
    for s in &r.services {
        let state = format!("{:?}", s.state).to_lowercase();
        out.push(format!("    service {}: {state}", s.name));
    }
    if let (Some(used), Some(max)) = (c.cache_used_mb, c.cache_max_mb) {
        out.push(format!("    cache {used}/{max} MB"));
    }
    for u in &r.unmet {
        out.push(format!("    unmet: {u}"));
    }
}

fn gigabytes(mb: u64) -> String {
    let text = format!("{:.1}", mb as f64 / 1024.0);
    match text.strip_suffix(".0") {
        Some(whole) => whole.to_string(),
        None => text,
    }
}

fn pool_header(p: &PoolInfo) -> String {
    let mut tags: Vec<&str> = p.held_tags.iter().map(|t| t.tag.as_str()).collect();
    tags.sort_unstable();
    tags.dedup();
    let held = if tags.is_empty() {
        "none".to_string()
    } else {
        tags.join(", ")
    };
    let memory = if p.mem_total_mb < 1024 {
        format!("{}/{} MB", p.mem_free_mb, p.mem_total_mb)
    } else if p.mem_free_mb < 1024 {
        format!("{} MB/{} GB", p.mem_free_mb, gigabytes(p.mem_total_mb))
    } else {
        format!(
            "{}/{} GB",
            gigabytes(p.mem_free_mb),
            gigabytes(p.mem_total_mb)
        )
    };
    format!(
        "  pool {}: memory {memory} free, cpu {}/{} free, tags held: {held}",
        p.name, p.cpu_free, p.cpu_total
    )
}

/// The runners of every broker as text, flat.
pub fn runner_lines(per: &[(String, Vec<RunnerInfo>)]) -> Vec<String> {
    let view: Vec<RunnerView> = per
        .iter()
        .map(|(b, r)| (b.clone(), r.clone(), Vec::new()))
        .collect();
    pool_runner_lines(&view)
}

/// The runners of every broker as text, grouped under the pool they draw on; a broker that
/// reports no pools prints the flat list.
pub fn pool_runner_lines(per: &[RunnerView]) -> Vec<String> {
    let mut out = Vec::new();
    for (b, runners, pools) in per {
        out.push(format!("broker {b}"));
        if runners.is_empty() {
            out.push("  (no runners)".into());
        }
        let mut shown = vec![false; runners.len()];
        for p in pools {
            out.push(pool_header(p));
            for (i, r) in runners.iter().enumerate() {
                if r.pool.as_deref() == Some(p.name.as_str()) {
                    shown[i] = true;
                    let mut block = Vec::new();
                    runner_block(r, &mut block);
                    out.extend(block.into_iter().map(|line| format!("  {line}")));
                }
            }
        }
        for (i, r) in runners.iter().enumerate() {
            if !shown[i] {
                runner_block(r, &mut out);
            }
        }
    }
    out
}

/// The document of `runners --json`.
pub fn runners_document(per: &[RunnerView], problems: &[String]) -> serde_json::Value {
    let brokers: Vec<_> = per
        .iter()
        .map(|(b, r, p)| json!({ "broker": b, "runners": r, "pools": p }))
        .collect();
    json!({ "brokers": brokers, "problems": problems })
}

/// Runners per reachable broker (only `broker` when given) and a line per broker that failed.
pub async fn fetch_runners(
    client: &IpcClient,
    broker: Option<&str>,
) -> Result<(Vec<(String, Vec<RunnerInfo>)>, Vec<String>), String> {
    let (view, problems) = fetch_runner_view(client, broker).await?;
    Ok((view.into_iter().map(|(b, r, _)| (b, r)).collect(), problems))
}

/// Runners and pools per reachable broker; a broker too old to report pools gives none.
pub async fn fetch_runner_view(
    client: &IpcClient,
    broker: Option<&str>,
) -> Result<(Vec<RunnerView>, Vec<String>), String> {
    let (names, mut problems) = reachable(client, broker).await?;
    let mut per = Vec::new();
    for b in names {
        match read(client, &b, ReadRequest::Runners).await {
            Ok(ReadReply::Runners { runners, pools }) => per.push((b, runners, pools)),
            Ok(other) => problems.push(problem(&b, &other)),
            Err(e) => problems.push(e),
        }
    }
    Ok((per, problems))
}

/// `runners [--broker B]`.
pub async fn runners(layout: &Layout, a: RunnersArgs) -> Result<i32, String> {
    let client = hub(layout).await?;
    let fetched = fetch_runner_view(&client, a.broker.as_deref()).await;
    client.close().await;
    let (per, problems) = fetched?;
    if a.json {
        let mut out = runners_document(&per, &problems);
        if let Some(lock) = crate::upgrade_lock::live_here(layout) {
            out["upgrade"] = json!(crate::upgrade_lock::describe(&lock));
        }
        print_json(&out)?;
    } else {
        if let Some(lock) = crate::upgrade_lock::live_here(layout) {
            println!("{}", crate::upgrade_lock::describe(&lock));
        }
        for line in pool_runner_lines(&per) {
            println!("{line}");
        }
        for p in &problems {
            eprintln!("{p}");
        }
    }
    Ok(0)
}
