use crate::common::{self, TempDir};
use lr_cli::commands::history_cmd::fetch_history;
use lr_cli::commands::jobs::hub;
use lr_cli::commands::read_cmd::fetch_runners;
use lr_cli::commands::view::{known_brokers, read};
use lr_cli::ipc_client::IpcClient;
use lr_cli::paths::Layout;
use lr_core::{Identity, JobId, JobState};
use lr_proto::{
    CliReply, CliRequest, HistoryFilter, JobSummary, ReadReply, ReadRequest, ReasonCode,
    RunnerInfo, StatusFilter, StatusView, SubmitRequest,
};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const LABEL: &str = "e2e";
pub const SECRET_NAME: &str = "e2e_token";
pub const SECRET_VAR: &str = "LR_E2E_SECRET_SOURCE";
/// Set for every process cargo runs by `[env]` of `.cargo/config.toml`, so the test never
/// changes its own environment while other test threads read it.
pub const SECRET_VALUE: &str = env!("LR_E2E_SECRET_SOURCE");

/// The runners read the test secret from the environment cargo gave this process.
pub fn init() {
    assert_eq!(
        std::env::var(SECRET_VAR).as_deref(),
        Ok(SECRET_VALUE),
        "run the end-to-end tests through cargo: {SECRET_VAR} comes from .cargo/config.toml"
    );
}

/// A machine of the test: its state and configuration directories under one temporary dir.
pub fn layout(dir: &TempDir, name: &str, policy_extra: &str) -> Layout {
    init();
    let root = dir.path().join(name).join("state");
    let config = dir.path().join(name).join("config");
    std::fs::create_dir_all(&config).unwrap();
    let user = lr_net::current_os_user().unwrap();
    let policy = format!(
        "[listen]\naddress = \"127.0.0.1:0\"\n\n[[local_user]]\nname = {user:?}\nlabel = \"{LABEL}\"\n\n\
         [[secrets.allow]]\nlabel = \"{LABEL}\"\nnames = [\"{SECRET_NAME}\"]\n\n{policy_extra}"
    );
    std::fs::write(config.join("policy.toml"), policy).unwrap();
    Layout::new(root, config)
}

/// The `runner.toml` of the test runners: jobs get the `PATH` of the test process through
/// `[env] path`, the only source of `PATH` of a job. Every machine of a test runs under the OS
/// user of the test next to a broker that admits other clients, so the runners accept that
/// (`allow_same_user_as_broker`, spec 3.12).
pub fn runner_toml() -> String {
    format!(
        "allow_elevated = true\nallow_same_user_as_broker = true\nmem_mb = 1024\ncpu = 1\n\n{}\n\
         [secret.{SECRET_NAME}]\nfrom = \"env:{SECRET_VAR}\"\n",
        common::env_path_toml()
    )
}

/// Creates runner `name` of `layout` with [`runner_toml`].
pub fn add_runner(layout: &Layout, name: &str) {
    let dir = layout.runner_dir(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("runner.toml"), runner_toml()).unwrap();
}

/// A git work tree with `.localrunner/profiles.toml`, committed.
pub fn repo(dir: &TempDir, profiles: &str) -> PathBuf {
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".localrunner")).unwrap();
    std::fs::write(repo.join(".localrunner").join("profiles.toml"), profiles).unwrap();
    common::init_repo(&repo);
    common::git(&repo, &["add", "."]);
    common::git(&repo, &["commit", "-q", "-m", "profiles"]);
    repo
}

/// A TOML string value for `text`.
pub fn toml_str(text: &str) -> String {
    toml::Value::String(text.to_string()).to_string()
}

/// Polls `probe` until it gives a value; panics after `limit`.
pub async fn eventually<T, F, Fut>(what: &str, limit: Duration, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let started = Instant::now();
    loop {
        if let Some(v) = probe().await {
            return v;
        }
        assert!(started.elapsed() < limit, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Submits `profile` from `repo` through the hub of `layout`; the channel stays open to follow
/// the job.
pub async fn submit(
    layout: &Layout,
    repo: &Path,
    profile: &str,
    platform: Option<&str>,
) -> (IpcClient, JobId) {
    let identity = common::session(LABEL, None, repo).expect("identity");
    submit_as(layout, identity, profile, platform, false).await
}

/// Submits `profile` with `identity` through the hub of `layout`, detached when `detach`.
pub async fn submit_as(
    layout: &Layout,
    identity: Identity,
    profile: &str,
    platform: Option<&str>,
    detach: bool,
) -> (IpcClient, JobId) {
    let client = hub(layout).await.expect("hub channel");
    let req = CliRequest::Submit(SubmitRequest {
        identity,
        profile: Some(profile.into()),
        platform: platform.map(str::to_string),
        timeout_s: None,
        mem_mb: None,
        cpu: None,
        detach,
        wait: true,
    });
    match client.call::<_, CliReply>(&req).await.expect("submit") {
        CliReply::Submitted { job, .. } => (client, job),
        other => panic!("submit answered {other:?}"),
    }
}

/// How a followed job ended.
#[derive(Debug)]
pub struct Ended {
    pub state: JobState,
    pub exit_code: Option<i32>,
    pub reason: Option<ReasonCode>,
    pub output: String,
}

/// How a followed job came out: it ended, or the hub refused it after `Submitted`.
#[derive(Debug)]
pub enum Outcome {
    Ended(Ended),
    Rejected(ReasonCode, String),
}

/// Reads the job's output from the hub until it ends or is refused, at most `limit`.
pub async fn until_end(client: IpcClient, job: &JobId, limit: Duration) -> Outcome {
    let mut output = String::new();
    let ended = tokio::time::timeout(limit, async {
        loop {
            match client.recv::<CliReply>().await.expect("hub answer") {
                CliReply::Output { job: j, text } if &j == job => output.push_str(&text),
                CliReply::Exit {
                    job: j,
                    state,
                    exit_code,
                    reason,
                } if &j == job => return Ok((state, exit_code, reason)),
                CliReply::Rejected { code, detail } => return Err((code, detail)),
                CliReply::Error(e) => panic!("hub error {e:?}"),
                _ => {}
            }
        }
    })
    .await;
    let Ok(ended) = ended else {
        panic!("job {job} did not end within {limit:?}; output so far: {output}");
    };
    client.close().await;
    match ended {
        Ok((state, exit_code, reason)) => Outcome::Ended(Ended {
            state,
            exit_code,
            reason,
            output,
        }),
        Err((code, detail)) => Outcome::Rejected(code, detail),
    }
}

/// Reads the job's output from the hub until it ends, at most `limit`.
pub async fn until_exit(client: IpcClient, job: &JobId, limit: Duration) -> Ended {
    match until_end(client, job, limit).await {
        Outcome::Ended(e) => e,
        Outcome::Rejected(code, detail) => panic!("rejected {code}: {detail}"),
    }
}

/// The refusal of a followed job, at most `limit`.
pub async fn until_rejected(
    client: IpcClient,
    job: &JobId,
    limit: Duration,
) -> (ReasonCode, String) {
    match until_end(client, job, limit).await {
        Outcome::Rejected(code, detail) => (code, detail),
        Outcome::Ended(e) => panic!("job {job} ran instead of being refused: {e:?}"),
    }
}

/// The local broker is known to the hub and available.
pub async fn broker_available(layout: &Layout) -> Option<()> {
    let client = hub(layout).await.ok()?;
    let brokers = known_brokers(&client).await.ok();
    client.close().await;
    brokers?.iter().any(|b| b.available).then_some(())
}

/// The runners the brokers of the hub of `layout` know.
pub async fn runners(layout: &Layout) -> Option<Vec<RunnerInfo>> {
    let client = hub(layout).await.ok()?;
    let fetched = fetch_runners(&client, None).await.ok();
    client.close().await;
    Some(fetched?.0.into_iter().flat_map(|(_, r)| r).collect())
}

/// Waits until a runner whose name ends with `name` is online.
pub async fn runner_online(layout: &Layout, name: &str) -> RunnerInfo {
    eventually(
        &format!("runner {name} online"),
        Duration::from_secs(60),
        || async {
            broker_available(layout).await?;
            runners(layout)
                .await?
                .into_iter()
                .find(|r| r.online && r.name.ends_with(name))
        },
    )
    .await
}

/// The status of the local broker.
pub async fn status(layout: &Layout) -> Option<StatusView> {
    let client = hub(layout).await.ok()?;
    let reply = read(
        &client,
        "local",
        ReadRequest::Status(StatusFilter::default()),
    )
    .await
    .ok();
    client.close().await;
    match reply? {
        ReadReply::Status(view) => Some(view),
        _ => None,
    }
}

/// Own runs of all brokers of the hub of `layout`.
pub async fn history(layout: &Layout) -> Vec<JobSummary> {
    history_of(layout, true).await
}

/// Runs of every client all brokers of the hub of `layout` show to it.
pub async fn all_history(layout: &Layout) -> Vec<JobSummary> {
    history_of(layout, false).await
}

async fn history_of(layout: &Layout, mine: bool) -> Vec<JobSummary> {
    let rows = try_history(layout, mine).await;
    rows.unwrap_or_else(|e| panic!("{e}"))
}

/// Runs the brokers of the hub of `layout` show (own ones when `mine`), or why some broker
/// could not answer.
pub async fn try_history(layout: &Layout, mine: bool) -> Result<Vec<JobSummary>, String> {
    let client = hub(layout).await?;
    let filter = HistoryFilter {
        mine,
        ..HistoryFilter::default()
    };
    let fetched = fetch_history(&client, None, &filter).await;
    client.close().await;
    let (per, problems) = fetched?;
    if !problems.is_empty() {
        return Err(format!("{problems:?}"));
    }
    Ok(per.into_iter().flat_map(|(_, jobs)| jobs).collect())
}

/// Waits until the helper of `role` has written its pid under `pids`.
pub async fn helper_pid(pids: &Path, role: &str) -> u32 {
    eventually(
        &format!("the {role} helper to start"),
        Duration::from_secs(30),
        || async { crate::helper::recorded_pid(pids, role) },
    )
    .await
}

/// Removes the test directory once the stopped roles and job processes let go of it: their
/// tasks end after the abort, so files can stay open for a moment (Windows refuses to delete
/// open files).
pub async fn remove(dir: TempDir) {
    let root = dir.path().parent().unwrap().to_path_buf();
    eventually(
        "the test directory to be removed",
        Duration::from_secs(20),
        || {
            let gone = match std::fs::remove_dir_all(&root) {
                Ok(()) => true,
                Err(e) => e.kind() == std::io::ErrorKind::NotFound,
            };
            async move { gone.then_some(()) }
        },
    )
    .await;
    drop(dir);
}
