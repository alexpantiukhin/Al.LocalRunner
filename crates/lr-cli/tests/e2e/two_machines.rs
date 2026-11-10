use crate::common::{self, TempDir};
use crate::support::*;
use lr_cli::commands::join_cmd::{runner_join, RunnerJoin};
use lr_cli::ipc_client::IpcClient;
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_core::JobState;
use lr_proto::{AdminReply, AdminRequest};
use std::path::Path;
use std::time::Duration;

const PLATFORM: &str = "e2e-remote";

async fn join_code(layout: &Layout) -> (String, String) {
    let admin = IpcClient::connect_broker_admin(&layout.admin_ipc())
        .await
        .expect("admin channel");
    let req = AdminRequest::BrokerAdd {
        name: None,
        label: None,
        runners: Some(1),
        address: None,
    };
    let reply: AdminReply = admin.call(&req).await.expect("admin reply");
    admin.close().await;
    match reply {
        AdminReply::JoinCode { address, code, .. } => (address, code),
        other => panic!("broker add answered {other:?}"),
    }
}

fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn join_runner_by_code_then_job_runs_remote() {
    let dir = TempDir::new("e2e-two");
    let main = layout(&dir, "main", "");
    let node = layout(&dir, "node", "");
    std::fs::write(node.config_file("runner.toml"), runner_toml()).unwrap();
    let broker = start_broker(&main, false).await.expect("broker starts");
    let hub_role = start_hub(&main, false).await.expect("hub starts");

    let (address, code) = join_code(&main).await;
    let join = RunnerJoin {
        address: address.clone(),
        code: code.clone(),
        name: Some("box".into()),
        count: 1,
        config: None,
        platforms: vec![PLATFORM.into()],
        tags: Vec::new(),
    };
    assert_eq!(runner_join(&node, join.clone()).await, Ok(0));
    let reused = RunnerJoin {
        name: Some("box2".into()),
        ..join
    };
    assert!(
        runner_join(&node, reused).await.is_err(),
        "a join code serves once"
    );

    let remote = start_runners(&node, Some("box"), 1, None, false)
        .await
        .expect("the joined runner starts");
    let info = runner_online(&main, "box").await;
    assert!(info.caps.platforms.contains(PLATFORM), "{info:?}");
    let known = runners(&main).await.expect("runners");
    assert_eq!(known.len(), 1, "only the joined runner: {known:?}");

    let profiles = format!(
        "[profile.remote]\ncommand = \"echo ran-on-the-joined-runner\"\ntimeout_s = 120\nmem_mb = 256\nplatforms = [\"{PLATFORM}\"]\n"
    );
    let repo = repo(&dir, &profiles);
    common::git(dir.path(), &["init", "-q", "--bare", "origin.git"]);
    let url = file_url(&dir.path().join("origin.git"));
    common::git(&repo, &["remote", "add", "origin", &url]);
    common::git(&repo, &["push", "-q", "-u", "origin", "HEAD"]);
    let (client, job) = submit(&main, &repo, "remote", Some(PLATFORM)).await;
    let ended = until_exit(client, &job, Duration::from_secs(120)).await;
    assert_eq!(ended.state, JobState::Passed, "{ended:?}");
    assert!(
        ended.output.contains("ran-on-the-joined-runner"),
        "{}",
        ended.output
    );
    let rows = history(&main).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0]
            .runner
            .as_deref()
            .is_some_and(|r| r.ends_with("box")),
        "{rows:?}"
    );

    remote.stop();
    hub_role.stop();
    broker.stop();
}
