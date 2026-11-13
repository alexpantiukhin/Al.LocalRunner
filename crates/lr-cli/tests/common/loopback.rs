use super::TempDir;
use lr_cli::commands::jobs::hub;
use lr_cli::commands::read_cmd::fetch_runners;
use lr_cli::commands::view::known_brokers;
use lr_cli::ipc_client::IpcClient;
use lr_cli::paths::Layout;
use lr_core::{JobId, JobState};
use lr_proto::{CliRead, CliReply, CliRequest, JobDetail, ReadReply, ReadRequest, SubmitRequest};
use std::path::PathBuf;
use std::time::Duration;

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
        "ping -n 120 127.0.0.1"
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
    for _ in 0..150 {
        let client = hub(layout).await.unwrap();
        let online = match fetch_runners(&client, None).await {
            Ok((per, _)) => per
                .iter()
                .flat_map(|(_, r)| r)
                .any(|r| r.online && r.name.ends_with("r1")),
            Err(_) => false,
        };
        let ready = online
            && known_brokers(&client)
                .await
                .is_ok_and(|b| b.iter().any(|b| b.available));
        client.close().await;
        if ready {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("runner r1 never came online");
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
    let mut last = None;
    for _ in 0..300 {
        let client = hub(layout).await.unwrap();
        let d = detail(&client, job).await;
        client.close().await;
        if let Some(d) = d {
            if d.summary.state == state {
                return d;
            }
            last = Some(d.summary.state);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{job} never became {state:?}, last {last:?}");
}
