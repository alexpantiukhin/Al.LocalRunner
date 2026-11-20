use super::TempDir;
use lr_cli::commands::jobs::hub;
use lr_cli::ipc_client::IpcClient;
use lr_cli::paths::Layout;
use lr_core::{JobId, JobState};
use lr_proto::{CliRead, CliReply, CliRequest, JobDetail, ReadReply, ReadRequest, SubmitRequest};
use std::path::PathBuf;

/// An installation in `dir` with a local user policy and runner `r1` (1 GB, 1 CPU).
pub fn layout(dir: &TempDir) -> Layout {
    let root = dir.path().join("state");
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    let user = lr_net::current_os_user().unwrap();
    std::fs::write(
        config.join("policy.toml"),
        format!("[[local_user]]\nname = {user:?}\n"),
    )
    .unwrap();
    let runner = root.join("runner").join("r1");
    std::fs::create_dir_all(&runner).unwrap();
    std::fs::write(
        runner.join("runner.toml"),
        format!(
            "allow_elevated = true\nmem_mb = 1024\ncpu = 1\n{}",
            super::env_path_toml()
        ),
    )
    .unwrap();
    Layout::new(root, config)
}

/// A git work tree in `dir` whose profile `slow` runs for two minutes.
pub fn repo(dir: &TempDir) -> PathBuf {
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".localrunner")).unwrap();
    let slow = if cfg!(windows) {
        "waitfor /t 120 lrslow"
    } else {
        "sleep 120"
    };
    std::fs::write(
        repo.join(".localrunner").join("profiles.toml"),
        format!("[profile.slow]\ncommand = \"{slow}\"\ntimeout_s = 300\nmem_mb = 256\n"),
    )
    .unwrap();
    super::init_repo(&repo);
    super::git(&repo, &["add", "."]);
    super::git(&repo, &["commit", "-q", "-m", "profiles"]);
    repo
}

/// Waits until runner `r1` is online and a broker is available through the hub.
pub async fn until_ready(layout: &Layout) {
    super::hub_events::until_online(layout, "r1").await;
}

/// Places profile `slow` for `identity`, detached.
pub async fn place(layout: &Layout, identity: lr_core::Identity) -> JobId {
    let client = hub(layout).await.expect("hub channel");
    let req = CliRequest::Submit(SubmitRequest {
        identity,
        profile: Some("slow".into()),
        platform: None,
        timeout_s: None,
        mem_mb: None,
        cpu: None,
        args: Vec::new(),
        zones: None,
        detach: true,
        wait: false,
    });
    let job = match client.call::<_, CliReply>(&req).await.expect("submit") {
        CliReply::Submitted { job, .. } => job,
        other => panic!("submit answered {other:?}"),
    };
    client
        .send(&CliRequest::Done { job: job.clone() })
        .await
        .unwrap();
    client.close().await;
    job
}

/// The record of `job` on the local broker, read through the hub.
pub async fn detail(client: &IpcClient, job: &JobId) -> Option<JobDetail> {
    let req = CliRequest::Read(CliRead {
        broker: Some("local".into()),
        request: ReadRequest::HistoryShow {
            job: job.clone(),
            client: None,
        },
    });
    match client.call::<_, CliReply>(&req).await.ok()? {
        CliReply::Read {
            reply: ReadReply::Job(d),
            ..
        } => Some(*d),
        _ => None,
    }
}

/// Waits until `job` is in `state` on the local broker and returns its record.
pub async fn until_state(layout: &Layout, job: &JobId, state: JobState) -> JobDetail {
    let what = format!("{job} to become {state:?}");
    super::hub_events::until_hub(layout, &what, || async {
        let client = hub(layout).await.ok()?;
        let d = detail(&client, job).await;
        client.close().await;
        d.filter(|d| d.summary.state == state)
    })
    .await
}
