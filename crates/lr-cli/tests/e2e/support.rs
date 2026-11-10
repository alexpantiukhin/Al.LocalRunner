use crate::common::{self, TempDir};
use lr_cli::commands::history_cmd::fetch_history;
use lr_cli::commands::jobs::hub;
use lr_cli::commands::read_cmd::fetch_runners;
use lr_cli::commands::view::{known_brokers, read};
use lr_cli::ipc_client::IpcClient;
use lr_cli::paths::Layout;
use lr_core::{JobId, JobState};
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

/// The `runner.toml` of the test runners.
pub fn runner_toml() -> String {
    format!(
        "allow_elevated = true\nmem_mb = 1024\ncpu = 1\n\n[secret.{SECRET_NAME}]\nfrom = \"env:{SECRET_VAR}\"\n"
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
/// the job when `wait`.
pub async fn submit(
    layout: &Layout,
    repo: &Path,
    profile: &str,
    platform: Option<&str>,
) -> (IpcClient, JobId) {
    let identity = common::session(LABEL, None, repo).expect("identity");
    let client = hub(layout).await.expect("hub channel");
    let req = CliRequest::Submit(SubmitRequest {
        identity,
        profile: Some(profile.into()),
        platform: platform.map(str::to_string),
        timeout_s: None,
        mem_mb: None,
        cpu: None,
        detach: false,
        wait: true,
    });
    match client.call::<_, CliReply>(&req).await.expect("submit") {
        CliReply::Submitted { job } => (client, job),
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

/// Reads the job's output from the hub until it ends, at most `limit`.
pub async fn until_exit(client: IpcClient, job: &JobId, limit: Duration) -> Ended {
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
                } if &j == job => return (state, exit_code, reason),
                CliReply::Rejected { code, detail } => panic!("rejected {code}: {detail}"),
                CliReply::Error(e) => panic!("hub error {e:?}"),
                _ => {}
            }
        }
    })
    .await;
    let Ok((state, exit_code, reason)) = ended else {
        panic!("job {job} did not end within {limit:?}; output so far: {output}");
    };
    client.close().await;
    Ended {
        state,
        exit_code,
        reason,
        output,
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
        Duration::from_secs(30),
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
    let client = hub(layout).await.expect("hub channel");
    let mine = HistoryFilter {
        mine: true,
        ..HistoryFilter::default()
    };
    let (per, problems) = fetch_history(&client, None, &mine).await.expect("history");
    client.close().await;
    assert!(problems.is_empty(), "{problems:?}");
    per.into_iter().flat_map(|(_, jobs)| jobs).collect()
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
