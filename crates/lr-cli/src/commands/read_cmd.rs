//! `explain`, `stats` and `runners` (spec 8, 9.1, 11.1): answers of the brokers through the hub,
//! as text or `--json`.

use super::jobs::hub;
use super::timefmt::{format_ms, format_time, now};
use super::view::{print_json, problem, reachable, read};
use super::{ExplainArgs, RunnersArgs, StatsArgs};
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_core::JobId;
use lr_proto::{ReadReply, ReadRequest, RunnerInfo, SpreadView, StatsFilter, StatsView};
use serde_json::json;

/// Why `job` waits, from the first broker that has it: the broker's name and its text, which
/// names the policy rule that holds the order.
pub async fn explain(
    client: &IpcClient,
    job: &JobId,
    broker: Option<&str>,
) -> Result<(String, String), String> {
    let (names, mut problems) = reachable(client, broker).await?;
    for b in names {
        let req = ReadRequest::Explain {
            job: job.clone(),
            client: None,
        };
        match read(client, &b, req).await {
            Ok(ReadReply::Explain { text, .. }) => return Ok((b, text)),
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

/// `explain <job>`.
pub async fn explain_cmd(layout: &Layout, a: ExplainArgs) -> Result<i32, String> {
    let client = hub(layout).await?;
    let job = JobId(a.job);
    let found = explain(&client, &job, a.broker.as_deref()).await;
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

/// The runners of every broker as text.
pub fn runner_lines(per: &[(String, Vec<RunnerInfo>)]) -> Vec<String> {
    let mut out = Vec::new();
    for (b, runners) in per {
        out.push(format!("broker {b}"));
        if runners.is_empty() {
            out.push("  (no runners)".into());
        }
        for r in runners {
            let c = &r.caps;
            out.push(format!(
                "  {}\t{}\tlast seen {}\tplatforms {}\tmemory {} MB, free in the pool {} MB\tcpu {}, free in the pool {}\t\
                 tags {}\tprograms {}",
                r.name,
                if r.online { "online" } else { "offline" },
                r.last_seen.map_or("-".into(), format_time),
                listed(c.platforms.iter()),
                c.mem_mb,
                r.free_mem_mb,
                c.cpu,
                r.free_cpu,
                listed(c.tags.iter()),
                listed(c.programs.iter()),
            ));
            if let Some(e) = &c.enforcement {
                out.push(format!("    enforcement {e}"));
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
    }
    out
}

/// Runners per reachable broker (only `broker` when given) and a line per broker that failed.
pub async fn fetch_runners(
    client: &IpcClient,
    broker: Option<&str>,
) -> Result<(Vec<(String, Vec<RunnerInfo>)>, Vec<String>), String> {
    let (names, mut problems) = reachable(client, broker).await?;
    let mut per = Vec::new();
    for b in names {
        match read(client, &b, ReadRequest::Runners).await {
            Ok(ReadReply::Runners { runners }) => per.push((b, runners)),
            Ok(other) => problems.push(problem(&b, &other)),
            Err(e) => problems.push(e),
        }
    }
    Ok((per, problems))
}

/// `runners [--broker B]`.
pub async fn runners(layout: &Layout, a: RunnersArgs) -> Result<i32, String> {
    let client = hub(layout).await?;
    let fetched = fetch_runners(&client, a.broker.as_deref()).await;
    client.close().await;
    let (per, problems) = fetched?;
    if a.json {
        let brokers: Vec<_> = per
            .iter()
            .map(|(b, r)| json!({ "broker": b, "runners": r }))
            .collect();
        print_json(&json!({ "brokers": brokers, "problems": problems }))?;
    } else {
        for line in runner_lines(&per) {
            println!("{line}");
        }
        for p in &problems {
            eprintln!("{p}");
        }
    }
    Ok(0)
}
