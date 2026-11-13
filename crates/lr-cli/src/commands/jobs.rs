//! `submit`, `run`, `wait`, `jobs` and `cancel` (spec 7, 11.1): the CLI collects the identity
//! and talks to its hub over the local CLI channel.

use super::confirm::{proceed, Console, Stdio};
use super::log_cmd::{cli_redactor, LogPrinter};
use super::OrderArgs;
use crate::facts::{collect_github, collect_session};
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_core::{Identity, JobId, JobState};
use lr_obs::Redactor;
use lr_proto::{
    CancelRequest, CancelTarget, CliReply, CliRequest, IpcChannel, ReasonCode, SubmitRequest,
};
use std::collections::BTreeMap;
use std::io::{IsTerminal, Read, Write};
use std::path::Path;
use std::sync::Arc;

/// Opens the CLI channel of the current OS user's hub.
pub async fn hub(layout: &Layout) -> Result<IpcClient, String> {
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    let address = layout.hub_ipc(&principal);
    IpcClient::connect(&address, IpcChannel::Cli)
        .await
        .map_err(|e| match e.starts_with(&format!("{address}: ")) {
            true => "the hub is not running; start it: localrunner up".to_string(),
            false => format!("the hub did not open its channel: {e}"),
        })
}

fn identity(label: Option<&str>, note: Option<&str>) -> Result<Identity, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let result = if env.get("GITHUB_ACTIONS").map(|v| v.trim()) == Some("true") {
        collect_github(&env, label, note, &cwd)
    } else {
        let label = label.ok_or("--as LABEL is required outside GitHub Actions (spec 7.1)")?;
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

/// How following a job ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The job finished.
    Exit {
        /// Final state.
        state: JobState,
        /// Exit code of the job.
        exit_code: Option<i32>,
        /// Reason code.
        reason: Option<ReasonCode>,
    },
    /// The hub refused the order.
    Rejected,
}

impl Ended {
    /// The exit code for the process.
    pub fn process_code(&self) -> i32 {
        match self {
            Ended::Exit {
                state,
                exit_code: code,
                ..
            } => exit_code(*state, *code),
            Ended::Rejected => 1,
        }
    }
}

/// Hands the output of `job` to `out` as it comes, whole lines masked by `redactor`, until the
/// job ends; the result is the exit code for the process.
pub async fn follow(
    client: &IpcClient,
    job: &JobId,
    redactor: &Arc<Redactor>,
    out: &mut dyn FnMut(&str),
) -> Result<i32, String> {
    Ok(follow_to_end(client, job, redactor, out)
        .await?
        .process_code())
}

/// [`follow`], returning how the job ended.
pub async fn follow_to_end(
    client: &IpcClient,
    job: &JobId,
    redactor: &Arc<Redactor>,
    out: &mut dyn FnMut(&str),
) -> Result<Ended, String> {
    let mut printer = LogPrinter::new(redactor.clone());
    loop {
        let reply = client.recv::<CliReply>().await;
        if !matches!(reply, Ok(CliReply::Output { .. })) {
            let rest = printer.finish();
            if !rest.is_empty() {
                out(&rest);
            }
        }
        match reply? {
            CliReply::Output { job: j, text } if &j == job => {
                let done = printer.push(&text);
                if !done.is_empty() {
                    out(&done);
                }
            }
            CliReply::Exit {
                job: j,
                state,
                exit_code,
                reason,
            } if &j == job => {
                let why = reason
                    .as_ref()
                    .map(|r| format!(", {r}"))
                    .unwrap_or_default();
                eprintln!("job {job}: {state:?}{why}");
                return Ok(Ended::Exit {
                    state,
                    exit_code,
                    reason,
                });
            }
            CliReply::Rejected { code, detail } => {
                eprintln!("job {job} rejected ({code}): {detail}");
                return Ok(Ended::Rejected);
            }
            CliReply::Error(e) => return Err(format!("{:?}: {}", e.code, e.message)),
            _ => continue,
        }
    }
}

/// The `wait --json` line: the job, its final state, exit code and reason (`null` when the
/// hub refused the order).
pub fn ended_json(job: &JobId, ended: &Ended) -> serde_json::Value {
    match ended {
        Ended::Exit {
            state,
            exit_code,
            reason,
        } => serde_json::json!({
            "job": job,
            "state": state,
            "exit_code": exit_code,
            "reason": reason,
        }),
        Ended::Rejected => serde_json::json!({
            "job": job,
            "state": null,
            "exit_code": null,
            "reason": null,
        }),
    }
}

async fn follow_to_stdout(layout: &Layout, client: &IpcClient, job: &JobId) -> Result<i32, String> {
    let mut stdout = std::io::stdout();
    let redactor = cli_redactor(layout);
    follow(client, job, &redactor, &mut |text| {
        let _ = stdout.write_all(text.as_bytes());
        let _ = stdout.flush();
    })
    .await
}

/// Writes the hub's warnings about a placed order to `err` (stderr), one per line, so the
/// JobId on stdout stays alone.
pub fn print_warnings(warnings: &[String], err: &mut impl Write) {
    for w in warnings {
        let _ = writeln!(err, "{w}");
    }
}

/// The line printed to stderr when an order is submitted without `--detach`, without `--wait`
/// and without a terminal.
pub const DETACH_WARNING: &str = "warning: without --detach this order is cancelled when this process exits; pass --detach and then run \"localrunner wait <job-id>\", or use \"localrunner run\"";

/// Writes `DETACH_WARNING` to `err` only when the order is neither detached, nor waited for,
/// nor submitted from a terminal.
pub fn warn_if_orphaned(detach: bool, wait: bool, terminal: bool, err: &mut impl Write) {
    if !detach && !wait && !terminal {
        let _ = writeln!(err, "{DETACH_WARNING}");
    }
}

/// `submit` (and `run` = `submit --wait`).
pub async fn submit(
    layout: &Layout,
    args: OrderArgs,
    wait: bool,
    json: bool,
) -> Result<i32, String> {
    let identity = identity(args.label.as_deref(), args.note.as_deref())?;
    let client = hub(layout).await?;
    let detach = args.detach;
    let req = SubmitRequest {
        identity,
        profile: args.profile,
        platform: args.platform,
        timeout_s: args.timeout,
        mem_mb: args.mem,
        cpu: args.cpu,
        args: args.args,
        detach,
        wait,
    };
    let mut stdout = std::io::stdout();
    let code = submit_on(client, req, json, &cli_redactor(layout), &mut |text| {
        let _ = stdout.write_all(text.as_bytes());
        let _ = stdout.flush();
    })
    .await?;
    if code == 0 {
        warn_if_orphaned(
            detach,
            wait,
            std::io::stdin().is_terminal(),
            &mut std::io::stderr(),
        );
    }
    Ok(code)
}

/// Places `req` through the open CLI channel `client`. Without `wait` it says `done` on the
/// same channel right after the JobId comes, so the hub never waits for this process, and
/// hands the JobId line to `out`; with `wait` it hands over the job's output instead.
pub async fn submit_on(
    client: IpcClient,
    req: SubmitRequest,
    json: bool,
    redactor: &Arc<Redactor>,
    out: &mut dyn FnMut(&str),
) -> Result<i32, String> {
    let wait = req.wait;
    let req = CliRequest::Submit(req);
    let job = match client.call::<_, CliReply>(&req).await? {
        CliReply::Submitted { job, warnings } => {
            print_warnings(&warnings, &mut std::io::stderr());
            job
        }
        CliReply::Rejected { code, detail } => {
            eprintln!("rejected ({code}): {detail}");
            return Ok(1);
        }
        other => return Err(unexpected(&other)),
    };
    if !wait {
        client.send(&CliRequest::Done { job: job.clone() }).await?;
        if json {
            out(&format!("{}\n", serde_json::json!({ "job": job })));
        } else {
            out(&format!("{job}\n"));
        }
        client.close().await;
        return Ok(0);
    }
    eprintln!("job {job}");
    follow(&client, &job, redactor, out).await
}

/// `wait <job> [--json]`: with `--json` the output goes to stderr and stdout gets one line,
/// [`ended_json`].
pub async fn wait(layout: &Layout, job: &str, json: bool) -> Result<i32, String> {
    let job = JobId(job.to_string());
    let client = hub(layout).await?;
    client.send(&CliRequest::Wait { job: job.clone() }).await?;
    if !json {
        return follow_to_stdout(layout, &client, &job).await;
    }
    let mut stderr = std::io::stderr();
    let ended = follow_to_end(&client, &job, &cli_redactor(layout), &mut |text| {
        let _ = stderr.write_all(text.as_bytes());
    })
    .await?;
    println!("{}", ended_json(&job, &ended));
    Ok(ended.process_code())
}

#[derive(serde::Deserialize)]
struct HandleLabel {
    label: String,
    submitted_at: u64,
}

const HANDLE_MAX_BYTES: u64 = 64 * 1024;

fn read_handle(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() > HANDLE_MAX_BYTES {
        return None;
    }
    let mut text = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(HANDLE_MAX_BYTES + 1)
        .read_to_end(&mut text)
        .ok()?;
    (text.len() as u64 <= HANDLE_MAX_BYTES).then_some(text)
}

/// The label of the handle in `<dir>/temp/runner/` with the latest submission time: the
/// default of `jobs --mine` without `--as` (spec 7.3). Only regular files of at most 64 KiB
/// count; links and other kinds of files are skipped.
pub fn label_from_last_handle(dir: &Path) -> Result<String, String> {
    let runner = dir.join("temp").join("runner");
    let missing = || {
        format!(
            "no --as and no handle in {} to take the label from; pass --as LABEL",
            runner.display()
        )
    };
    let entries = std::fs::read_dir(&runner).map_err(|_| missing())?;
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let h: HandleLabel = serde_json::from_slice(&read_handle(&p)?).ok()?;
            Some((h.submitted_at, p, h.label))
        })
        .max()
        .map(|(_, _, label)| label)
        .ok_or_else(missing)
}

/// `jobs --mine [--active]`; without `label`, the label of the last handle in the current
/// directory.
pub async fn jobs(
    layout: &Layout,
    label: Option<String>,
    active: bool,
    json: bool,
) -> Result<i32, String> {
    let label = match label {
        Some(l) => l,
        None => {
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            label_from_last_handle(&cwd)?
        }
    };
    let client = hub(layout).await?;
    let req = CliRequest::Jobs { label, active };
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
            println!("{}", mine_line(j));
        }
    }
    Ok(0)
}

/// One line of `jobs --mine`: job, state, profile, then project, source and label together
/// (spec 7.2); an unknown value is `-`.
pub fn mine_line(j: &lr_proto::JobSummary) -> String {
    let mut line = format!(
        "{}\t{:?}\t{}\t{}\t{}\t{}",
        j.job,
        j.state,
        j.profile.as_deref().unwrap_or("-"),
        j.project.as_deref().unwrap_or("-"),
        super::view::source_name(j.source),
        j.label.as_deref().unwrap_or("-")
    );
    if j.peak_mem_mb.is_some() || j.peak_disk_mb.is_some() {
        let mb = |v: Option<u64>| v.map_or("-".to_string(), |v| v.to_string());
        line.push_str(&format!(
            "\tpeak mem {} MB, disk {} MB",
            mb(j.peak_mem_mb),
            mb(j.peak_disk_mb)
        ));
    }
    line
}

/// `cancel` as the command line runs it (see [`cancel_asked`]).
pub async fn cancel_cli(
    layout: &Layout,
    job: Option<String>,
    label: &str,
    mine: bool,
    force: bool,
    reason: Option<String>,
    yes: bool,
) -> Result<i32, String> {
    let mut stdio = Stdio::new();
    let mut console = stdio.console();
    cancel_asked(layout, job, label, mine, force, reason, yes, &mut console).await
}

/// [`cancel`] that, for `--mine` (it can cancel several jobs), first asks (`yes` answers); a
/// refusal or a "no" ends with code 1 before the hub is reached. One named job never asks.
#[allow(clippy::too_many_arguments)]
pub async fn cancel_asked(
    layout: &Layout,
    job: Option<String>,
    label: &str,
    mine: bool,
    force: bool,
    reason: Option<String>,
    yes: bool,
    console: &mut Console<'_>,
) -> Result<i32, String> {
    if mine
        && !proceed(
            "cancel --mine cancels all your active jobs",
            &format!("cancel all active jobs of label {label}?"),
            yes,
            console,
        )?
    {
        return Ok(1);
    }
    cancel(layout, job, label, mine, force, reason).await
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
    let actor = identity(Some(label), None)?;
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
