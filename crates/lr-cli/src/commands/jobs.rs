//! `submit`, `run`, `wait`, `jobs` and `cancel` (spec 7, 11.1): the CLI collects the identity
//! and talks to its hub over the local CLI channel.

use super::OrderArgs;
use crate::facts::{collect_github, collect_session};
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_core::{Identity, JobId, JobState};
use lr_proto::{CancelRequest, CancelTarget, CliReply, CliRequest, IpcChannel, SubmitRequest};
use std::collections::BTreeMap;
use std::io::Write;

/// Opens the CLI channel of the current OS user's hub.
pub async fn hub(layout: &Layout) -> Result<IpcClient, String> {
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    IpcClient::connect(&layout.hub_ipc(&principal), IpcChannel::Cli)
        .await
        .map_err(|e| format!("the hub is not reachable ({e}); start it: localrunner up"))
}

fn identity(label: &str, note: Option<&str>) -> Result<Identity, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let result = if env.get("GITHUB_ACTIONS").map(String::as_str) == Some("true") {
        collect_github(&env, Some(label), note, &cwd)
    } else {
        collect_session(label, note, &cwd)
    };
    result.map_err(|e| e.to_string())
}

/// The process exit code of a finished job: 0 when it passed, its own non-zero code when it
/// failed with one, 1 otherwise.
pub fn exit_code(state: JobState, code: Option<i32>) -> i32 {
    match state {
        JobState::Passed => 0,
        _ => code.filter(|c| *c != 0).unwrap_or(1),
    }
}

fn unexpected(reply: &CliReply) -> String {
    match reply {
        CliReply::Error(e) => format!("{:?}: {}", e.code, e.message),
        CliReply::Rejected { code, detail } => format!("rejected ({code}): {detail}"),
        other => format!("unexpected answer from the hub: {other:?}"),
    }
}

async fn follow(client: &IpcClient, job: &JobId) -> Result<i32, String> {
    let mut out = std::io::stdout();
    loop {
        match client.recv::<CliReply>().await? {
            CliReply::Output { job: j, text } if &j == job => {
                let _ = out.write_all(text.as_bytes());
                let _ = out.flush();
            }
            CliReply::Exit {
                job: j,
                state,
                exit_code: code,
                reason,
            } if &j == job => {
                let reason = reason.map(|r| format!(", {r}")).unwrap_or_default();
                eprintln!("job {job}: {state:?}{reason}");
                return Ok(exit_code(state, code));
            }
            CliReply::Rejected { code, detail } => {
                eprintln!("job {job} rejected ({code}): {detail}");
                return Ok(1);
            }
            CliReply::Error(e) => return Err(format!("{:?}: {}", e.code, e.message)),
            _ => continue,
        }
    }
}

/// `submit` (and `run` = `submit --wait`).
pub async fn submit(
    layout: &Layout,
    args: OrderArgs,
    wait: bool,
    json: bool,
) -> Result<i32, String> {
    let identity = identity(&args.label, args.note.as_deref())?;
    let client = hub(layout).await?;
    let req = CliRequest::Submit(SubmitRequest {
        identity,
        profile: args.profile,
        platform: args.platform,
        timeout_s: args.timeout,
        detach: args.detach,
        wait,
    });
    let job = match client.call::<_, CliReply>(&req).await? {
        CliReply::Submitted { job } => job,
        CliReply::Rejected { code, detail } => {
            eprintln!("rejected ({code}): {detail}");
            return Ok(1);
        }
        other => return Err(unexpected(&other)),
    };
    if !wait {
        client.send(&CliRequest::Done { job: job.clone() }).await?;
        if json {
            println!("{}", serde_json::json!({ "job": job }));
        } else {
            println!("{job}");
        }
        client.close().await;
        return Ok(0);
    }
    eprintln!("job {job}");
    follow(&client, &job).await
}

/// `wait <job>`.
pub async fn wait(layout: &Layout, job: &str) -> Result<i32, String> {
    let job = JobId(job.to_string());
    let client = hub(layout).await?;
    client.send(&CliRequest::Wait { job: job.clone() }).await?;
    follow(&client, &job).await
}

/// `jobs --mine [--active]`.
pub async fn jobs(layout: &Layout, label: &str, active: bool, json: bool) -> Result<i32, String> {
    let client = hub(layout).await?;
    let req = CliRequest::Jobs {
        label: label.to_string(),
        active,
    };
    let jobs = match client.call::<_, CliReply>(&req).await? {
        CliReply::Jobs { jobs } => jobs,
        other => return Err(unexpected(&other)),
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&jobs).map_err(|e| e.to_string())?
        );
    } else {
        for j in &jobs {
            println!(
                "{}\t{:?}\t{}\t{}",
                j.job,
                j.state,
                j.profile.as_deref().unwrap_or("-"),
                j.project.as_deref().unwrap_or("-")
            );
        }
    }
    Ok(0)
}

/// `cancel <job>` or `cancel --mine`.
pub async fn cancel(
    layout: &Layout,
    job: Option<String>,
    label: &str,
    mine: bool,
    force: bool,
    reason: Option<String>,
) -> Result<i32, String> {
    let target = match (job, mine) {
        (Some(j), false) => CancelTarget::Job(JobId(j)),
        (None, true) => CancelTarget::Mine,
        _ => return Err("name one job or give --mine".into()),
    };
    let actor = identity(label, None)?;
    let client = hub(layout).await?;
    let req = CliRequest::Cancel(CancelRequest {
        actor,
        target,
        force,
        reason,
    });
    match client.call::<_, CliReply>(&req).await? {
        CliReply::Cancelled { jobs } => {
            for j in jobs {
                println!("{j}");
            }
            Ok(0)
        }
        other => Err(unexpected(&other)),
    }
}
