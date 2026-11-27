//! `explain`, `stats` and `runners` (spec 8, 9.1, 11.1): answers of the brokers through the hub,
//! as text or `--json`.

use super::jobs::hub;
use super::timefmt::{format_ms, format_time, now};
use super::view::{is_plain_missing, missing_job_error, print_json, problem, reachable, read};
use super::{ExplainArgs, RunnersArgs, StatsArgs};
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use crate::render::{Card, Cell, Out, Table, Tone};
use lr_core::JobId;
use lr_proto::{
    PoolInfo, ReadReply, ReadRequest, RunnerInfo, RunnerKind, RunnerLiveness, SpreadView,
    StatsFilter, StatsView, WaitDetail,
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
    let (broker, text, _) = explain_with_detail(client, job, broker, run_client).await?;
    Ok((broker, text))
}

/// [`explain_for`] with the structured part of the answer: who the order waits for and the
/// upper bound of the wait, when the broker gave them.
pub async fn explain_with_detail(
    client: &IpcClient,
    job: &JobId,
    broker: Option<&str>,
    run_client: Option<String>,
) -> Result<(String, String, Option<WaitDetail>), String> {
    let (names, mut problems) = reachable(client, broker).await?;
    let mut ambiguous = Vec::new();
    for b in names {
        let req = ReadRequest::Explain {
            job: job.clone(),
            client: run_client.clone(),
        };
        match read(client, &b, req).await {
            Ok(ReadReply::Explain { text, detail, .. }) => return Ok((b, text, detail)),
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
    let found = explain_with_detail(&client, &job, a.broker.as_deref(), a.client).await;
    client.close().await;
    let (broker, text, detail) = found?;
    if a.json {
        let mut doc = json!({ "broker": broker, "job": job, "text": text });
        if let Some(detail) = detail {
            doc["detail"] = json!(detail);
        }
        print_json(&doc)?;
    } else {
        let mut card = Card::new();
        card.row("job", job.to_string());
        card.row("broker", broker);
        card.row("waits", text);
        for line in card.lines(&Out::detect()) {
            println!("{line}");
        }
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

fn memory_line(m: &lr_proto::MemoryStatsView) -> String {
    let mb = |v: Option<u64>| v.map_or("-".to_string(), |v| format!("{v} MB"));
    let verdict = match (m.declared_mb, m.max_mb) {
        (Some(d), Some(max)) if max > d => " (the peak is above the declared value: raise mem_mb)",
        (Some(d), Some(max)) if max.saturating_mul(2) < d => {
            " (the declared value is more than twice the peak: it can be lowered)"
        }
        _ => "",
    };
    format!(
        "memory: declared {}; peak median {}, max {} over {} runs{verdict}",
        mb(m.declared_mb),
        mb(m.median_mb),
        mb(m.max_mb),
        m.measured
    )
}

/// The statistics of every broker as plain text; see [`stats_lines_for`].
pub fn stats_lines(per: &[(String, StatsView)]) -> Vec<String> {
    stats_lines_for(per, &Out::plain())
}

/// The statistics of every broker: per broker a table of its profiles (a failure count in red),
/// the memory verdict and the steps of a profile on lines under it, then a table of the clients.
pub fn stats_lines_for(per: &[(String, StatsView)], out: &Out) -> Vec<String> {
    let mut lines = Vec::new();
    for (b, v) in per {
        lines.push(out.paint(&format!("broker {b}"), Tone::Head));
        if v.profiles.is_empty() && v.clients.is_empty() {
            lines.push("  (no finished runs)".into());
        }
        if !v.profiles.is_empty() {
            let mut table = Table::new(&[
                "PROFILE",
                "RUNS",
                "PASSED",
                "FAILED",
                "TIMEOUTS",
                "CANCELLED",
                "LOST",
                "WAIT",
                "DURATION",
            ])
            .right(1)
            .right(2)
            .right(3)
            .right(4)
            .right(5)
            .right(6)
            .fixed(0);
            let mut notes: Vec<Vec<String>> = Vec::new();
            for p in &v.profiles {
                let count = |n: u64, tone: Tone| {
                    if n > 0 {
                        Cell::new(n.to_string(), tone)
                    } else {
                        Cell::plain(n.to_string())
                    }
                };
                table.row(vec![
                    Cell::plain(p.profile.clone()),
                    Cell::plain(p.runs.to_string()),
                    Cell::plain(share(p.passed, p.runs)),
                    count(p.failed, Tone::Bad),
                    count(p.timeouts, Tone::Bad),
                    count(p.cancelled, Tone::Warn),
                    count(p.lost, Tone::Bad),
                    Cell::plain(spread(&p.wait_ms)),
                    Cell::plain(spread(&p.duration_ms)),
                ]);
                let mut sub = Vec::new();
                if let Some(m) = p
                    .mem
                    .as_ref()
                    .filter(|m| m.declared_mb.is_some() || m.measured > 0)
                {
                    sub.push(memory_line(m));
                }
                for s in &p.steps {
                    sub.push(format!(
                        "step {}: {} runs, {} passed; duration {}",
                        s.name,
                        s.runs,
                        share(s.passed, s.runs),
                        spread(&s.duration_ms)
                    ));
                }
                notes.push(sub);
            }
            let rendered = table.lines(out);
            lines.push(format!("  {}", rendered[0]));
            for (row, sub) in rendered[1..].iter().zip(notes) {
                lines.push(format!("  {row}"));
                for n in sub {
                    lines.push(format!("    {}", out.paint(&n, Tone::Dim)));
                }
            }
        }
        if !v.clients.is_empty() {
            let mut table = Table::new(&["CLIENT", "LABEL", "RUNS", "BUSY"])
                .right(2)
                .fixed(2)
                .fixed(3);
            for c in &v.clients {
                table.row(vec![
                    Cell::plain(c.client.clone()),
                    Cell::plain(c.label.clone()),
                    Cell::plain(c.runs.to_string()),
                    Cell::plain(format_ms(c.busy_ms)),
                ]);
            }
            lines.extend(table.lines(out).into_iter().map(|l| format!("  {l}")));
        }
    }
    lines
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
        for line in stats_lines_for(&per, &Out::detect()) {
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

/// `programs not found: a, b`: what `runners` and `status` say next to a runner that declares
/// programs it did not find; the runner still shows as online.
pub fn missing_programs_text(missing: &std::collections::BTreeSet<String>) -> String {
    format!(
        "programs not found: {}",
        missing
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Whether a runner that reports `runner` as its version lags behind `current`: it is older,
/// or it reports none (a runner of an earlier version). A version that does not parse is not
/// judged.
pub fn version_behind(runner: Option<&str>, current: &str) -> bool {
    use super::upgrade_cmd::parse_version;
    let Some(current) = parse_version(current) else {
        return false;
    };
    match runner {
        None => true,
        Some(v) => parse_version(v).is_some_and(|v| v.number < current.number),
    }
}

/// What `runners` and `status` say about a runner whose version lags behind.
pub fn behind_text(runner: Option<&str>, current: &str) -> String {
    format!(
        "version {} is behind {current}: run `localrunner upgrade` on its machine",
        runner.unwrap_or("unknown")
    )
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

fn kind_text(r: &RunnerInfo) -> Option<&'static str> {
    match r.kind {
        Some(RunnerKind::Local) => Some("local"),
        Some(RunnerKind::Joined) => Some("joined"),
        Some(RunnerKind::Unknown) => Some("?"),
        None => None,
    }
}

fn state_cell(r: &RunnerInfo) -> Cell {
    let tone = match (r.liveness, r.online) {
        (Some(RunnerLiveness::Stale), _) => Tone::Warn,
        (Some(RunnerLiveness::Online), _) | (None, true) => Tone::Ok,
        _ => Tone::Bad,
    };
    Cell::new(liveness_text(r), tone)
}

fn version_cell(r: &RunnerInfo, mine: &str) -> Cell {
    let behind = version_behind(r.caps.version.as_deref(), mine);
    let tone = if behind { Tone::Warn } else { Tone::Plain };
    Cell::new(r.caps.version.as_deref().unwrap_or("unknown"), tone)
}

fn ready_cell(r: &RunnerInfo) -> Cell {
    match (r.can_start_now, r.busy) {
        (Some(true), _) => Cell::ok("yes"),
        (Some(false), Some(true)) => Cell::warn("busy"),
        (Some(false), _) => Cell::warn("no"),
        (None, _) => Cell::dim("-"),
    }
}

fn has_details(r: &RunnerInfo) -> bool {
    !r.held_tags.is_empty()
        || !r.services.is_empty()
        || !r.unmet.is_empty()
        || !r.caps.programs_missing.is_empty()
}

/// The table of the runners of one pool or broker; `wide` adds programs, enforcement and cache.
pub fn runner_table(runners: &[&RunnerInfo], wide: bool) -> Table {
    let mine = env!("CARGO_PKG_VERSION");
    let mut headers = vec![
        "NAME", "STATE", "KIND", "ZONE", "VERSION", "PLATFORM", "MEMORY", "CPU", "DISK", "TAGS", "READY",
    ];
    if wide {
        headers.extend(["PROGRAMS", "ENFORCEMENT", "CACHE"]);
    }
    let mut table = Table::new(&headers)
        .fixed(0)
        .fixed(1)
        .fixed(2)
        .fixed(3)
        .fixed(4)
        .fixed(6)
        .fixed(7)
        .fixed(8)
        .fixed(10);
    for r in runners {
        let c = &r.caps;
        let mut row = vec![
            Cell::plain(r.name.clone()),
            state_cell(r),
            kind_text(r).map_or_else(|| Cell::dim("-"), Cell::plain),
            r.zone.map_or_else(|| Cell::dim("-"), |z| Cell::plain(z.as_str())),
            version_cell(r, mine),
            Cell::plain(listed(c.platforms.iter())),
            Cell::plain(memory_pair(r.free_mem_mb, c.mem_mb)),
            Cell::plain(format!("{}/{}", r.free_cpu, c.cpu)),
            Cell::plain(format!("{} GB", c.disk_mb / 1024)),
            Cell::plain(listed(c.tags.iter())),
            ready_cell(r),
        ];
        if wide {
            row.push(Cell::plain(listed(c.programs.iter())));
            row.push(Cell::plain(c.enforcement.as_deref().unwrap_or("unknown")));
            row.push(match (c.cache_used_mb, c.cache_max_mb) {
                (Some(used), Some(max)) => Cell::plain(format!("{used}/{max} MB")),
                _ => Cell::dim("-"),
            });
        }
        table.row(row);
    }
    table
}

/// Everything known about one runner, a label and a value per line.
pub fn runner_card(r: &RunnerInfo) -> Card {
    let c = &r.caps;
    let mine = env!("CARGO_PKG_VERSION");
    let mut card = Card::new();
    card.row("name", r.name.clone());
    card.row("state", state_cell(r));
    if let Some(kind) = kind_text(r) {
        card.row("kind", kind);
    }
    card.row("last seen", r.last_seen.map_or("-".into(), format_time));
    card.row("platforms", listed(c.platforms.iter()));
    let version = match c.version.as_deref() {
        Some(v) if v == mine => Cell::plain(v),
        Some(v) => Cell::warn(format!("{v} (this binary: {mine})")),
        None => Cell::warn(format!("unknown (this binary: {mine})")),
    };
    card.row("version", version);
    if version_behind(c.version.as_deref(), mine) {
        card.row("", Cell::warn(behind_text(c.version.as_deref(), mine)));
    }
    card.row(
        "memory",
        format!("{} MB, free in the pool {} MB", c.mem_mb, r.free_mem_mb),
    );
    card.row("cpu", format!("{}, free in the pool {}", c.cpu, r.free_cpu));
    card.row("disk", format!("{} GB free", c.disk_mb / 1024));
    card.row("tags", listed(c.tags.iter()));
    card.row("programs", listed(c.programs.iter()));
    if !c.programs_missing.is_empty() {
        card.row("", Cell::warn(missing_programs_text(&c.programs_missing)));
    }
    if let (Some(busy), Some(can)) = (r.busy, r.can_start_now) {
        card.row(
            "ready",
            format!(
                "{}; can start a job now: {}",
                if busy { "busy" } else { "idle" },
                if can { "yes" } else { "no" }
            ),
        );
    }
    let older = if c.enforce_limit {
        ""
    } else {
        match c.enforcement.as_deref() {
            Some("watcher") => " (limit mode not supported: runs as warn; no systemd here, so the memory of a job is not limited; in a container the total limit is set by Docker: mem_limit, cpus)",
            _ => " (limit mode not supported: runs as warn; an earlier version, or the runner has not reported it yet)",
        }
    };
    card.row(
        "enforcement",
        format!("{}{older}", c.enforcement.as_deref().unwrap_or("unknown")),
    );
    for t in &r.held_tags {
        let kind = if t.exclusive { ", exclusive" } else { "" };
        let holder = if t.job.0.is_empty() {
            String::new()
        } else {
            format!(" by {}", t.job)
        };
        card.row("tag", format!("{} held{holder}{kind}", t.tag));
    }
    for s in &r.services {
        let state = format!("{:?}", s.state).to_lowercase();
        card.row("service", format!("{}: {state}", s.name));
    }
    if let (Some(used), Some(max)) = (c.cache_used_mb, c.cache_max_mb) {
        card.row("cache", format!("{used}/{max} MB"));
    }
    for u in &r.unmet {
        card.row("unmet", Cell::bad(u.clone()));
    }
    card
}

fn gigabytes(mb: u64) -> String {
    let text = format!("{:.1}", mb as f64 / 1024.0);
    match text.strip_suffix(".0") {
        Some(whole) => whole.to_string(),
        None => text,
    }
}

fn memory_pair(free_mb: u64, total_mb: u64) -> String {
    if total_mb < 1024 {
        format!("{free_mb}/{total_mb} MB")
    } else if free_mb < 1024 {
        format!("{free_mb} MB/{} GB", gigabytes(total_mb))
    } else {
        format!("{}/{} GB", gigabytes(free_mb), gigabytes(total_mb))
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
    let memory = memory_pair(p.mem_free_mb, p.mem_total_mb);
    format!(
        "  pool {}: memory {memory} free, cpu {}/{} free, tags held: {held}",
        p.name, p.cpu_free, p.cpu_total
    )
}

/// The runners of every broker as plain text, flat.
pub fn runner_lines(per: &[(String, Vec<RunnerInfo>)]) -> Vec<String> {
    let view: Vec<RunnerView> = per
        .iter()
        .map(|(b, r)| (b.clone(), r.clone(), Vec::new()))
        .collect();
    pool_runner_lines(&view)
}

/// The runners of every broker as plain text, one table per pool.
pub fn pool_runner_lines(per: &[RunnerView]) -> Vec<String> {
    runner_listing(per, &Out::plain(), false)
}

fn indented(lines: Vec<String>) -> impl Iterator<Item = String> {
    lines.into_iter().map(|l| format!("  {l}"))
}

/// The runners of every broker for `out`: per broker its name, then per pool the pool line and
/// a table of its runners (a broker that reports no pools has one table), then a note for
/// every runner whose version lags behind. `wide` adds the columns that are cards otherwise.
pub fn runner_listing(per: &[RunnerView], out: &Out, wide: bool) -> Vec<String> {
    let mine = env!("CARGO_PKG_VERSION");
    let mut lines = Vec::new();
    for (b, runners, pools) in per {
        lines.push(out.paint(&format!("broker {b}"), Tone::Head));
        if runners.is_empty() {
            lines.push("  (no runners)".into());
            continue;
        }
        let mut shown = vec![false; runners.len()];
        for p in pools {
            lines.push(format!("  {}", pool_header(p).trim_start()));
            let members: Vec<&RunnerInfo> = runners
                .iter()
                .enumerate()
                .filter(|(_, r)| r.pool.as_deref() == Some(p.name.as_str()))
                .map(|(i, r)| {
                    shown[i] = true;
                    r
                })
                .collect();
            lines.extend(indented(runner_table(&members, wide).lines(out)));
        }
        let rest: Vec<&RunnerInfo> = runners
            .iter()
            .enumerate()
            .filter(|(i, _)| !shown[*i])
            .map(|(_, r)| r)
            .collect();
        lines.extend(indented(runner_table(&rest, wide).lines(out)));
        for r in runners {
            if version_behind(r.caps.version.as_deref(), mine) {
                let note = format!(
                    "{}: {}",
                    r.name,
                    behind_text(r.caps.version.as_deref(), mine)
                );
                lines.push(format!("  {}", out.paint(&note, Tone::Warn)));
            }
        }
        if !wide && runners.iter().any(has_details) {
            let hint = "more of one runner: localrunner runners <name>; every column: --wide";
            lines.push(format!("  {}", out.paint(hint, Tone::Dim)));
        }
    }
    lines
}

/// The cards of the runners called `name`, per broker that has one.
pub fn runner_cards(per: &[RunnerView], name: &str, out: &Out) -> Vec<String> {
    let mut lines = Vec::new();
    for (b, runners, _) in per {
        for r in runners.iter().filter(|r| r.name == name) {
            lines.push(out.paint(&format!("broker {b}"), Tone::Head));
            lines.extend(indented(runner_card(r).lines(out)));
        }
    }
    lines
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
        let out = Out::detect();
        let lines = match a.name.as_deref() {
            Some(name) => {
                let cards = runner_cards(&per, name, &out);
                if cards.is_empty() {
                    return Err(format!("no runner called {name} on the brokers asked"));
                }
                cards
            }
            None => runner_listing(&per, &out, a.wide),
        };
        for line in lines {
            println!("{line}");
        }
        for p in &problems {
            eprintln!("{p}");
        }
    }
    Ok(0)
}
