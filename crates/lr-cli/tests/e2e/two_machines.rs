use crate::common::{self, TempDir};
use crate::kit::*;
use crate::support::*;
use lr_cli::commands::join_cmd::{runner_join, RunnerJoin};
use lr_cli::ipc_client::IpcClient;
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_core::{JobState, Zone};
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

// scenario: 16.2
// scenario: 16.12
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
    let refused = runner_join(&node, reused)
        .await
        .expect_err("a join code serves once");
    assert!(
        refused.contains("join_code_unknown"),
        "the used code is unknown to the broker: {refused}"
    );
    assert!(!node.runner_dir("box2").join("broker.toml").exists());

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
    remove(dir).await;
}

// scenario: 16.3
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_joins_by_code_with_the_owner_label_and_its_orders_carry_it() {
    let dir = TempDir::new("e2e-hj");
    let (main, roles) = machine(
        &dir,
        "main",
        "[networks]\nhere = [\"127.0.0.0/8\", \"::1/128\"]\n",
        &["r1"],
        &platform_runner_toml(),
    )
    .await;
    let own = brokers(&main).await;
    assert!(
        own.iter()
            .any(|b| b.name == "local" && b.available && b.zone == Some(Zone::Local)),
        "the hub of the broker's machine needs no code: {own:?}"
    );
    let (laptop, laptop_hub) = joined_hub(&dir, &main, "laptop", "laptop", Some("ci")).await;
    let seen = brokers(&laptop).await;
    let joined = seen
        .iter()
        .find(|b| b.name == "main")
        .unwrap_or_else(|| panic!("the joined broker is known by the name of the join: {seen:?}"));
    assert_eq!(
        joined.zone,
        Some(Zone::Lan),
        "the zone comes from the address of the connection: {seen:?}"
    );

    let profiles = format!(
        "{}\n{}",
        echo_profile(
            "remote",
            "from-the-laptop",
            120,
            &format!("mem_mb = 256\nplatforms = [\"{PLATFORM}\"]\n")
        ),
        echo_profile("here", "from-the-broker-machine", 60, "mem_mb = 256\n"),
    );
    let repo = repo(&dir, &profiles);
    push_origin(&dir, &repo);
    let (client, from_laptop) = submit(&laptop, &repo, "remote", Some(PLATFORM)).await;
    let ended = until_exit(client, &from_laptop, Duration::from_secs(120)).await;
    assert_eq!(ended.state, JobState::Passed, "{ended:?}");
    assert!(ended.output.contains("from-the-laptop"), "{}", ended.output);
    let (client, from_main) = submit(&main, &repo, "here", None).await;
    let ended = until_exit(client, &from_main, Duration::from_secs(60)).await;
    assert_eq!(ended.state, JobState::Passed, "{ended:?}");

    let laptop_row = row(&main, &from_laptop).await;
    assert_eq!(
        laptop_row.client_label, "ci",
        "the order carries the label the owner gave with the code: {laptop_row:?}"
    );
    let main_row = row(&main, &from_main).await;
    assert_eq!(main_row.client_label, LABEL, "{main_row:?}");

    laptop_hub.stop();
    roles.stop();
    remove(dir).await;
}

// scenario: 16.6
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_code_for_three_runners_refuses_the_fourth() {
    let dir = TempDir::new("e2e-n3");
    let (main, roles) = machine(&dir, "main", "", &[], "").await;
    let node = layout(&dir, "node", "");
    std::fs::write(node.config_file("runner.toml"), runner_toml()).unwrap();
    let (address, code) = kit_code(&main, 3).await;
    let join = RunnerJoin {
        address,
        code,
        name: Some("heavy".into()),
        count: 3,
        config: None,
        platforms: vec![PLATFORM.into()],
        tags: Vec::new(),
    };
    assert_eq!(runner_join(&node, join.clone()).await, Ok(0));
    let mut made = node.runner_names();
    made.sort();
    assert_eq!(made, ["heavy-1", "heavy-2", "heavy-3"]);
    let keys: std::collections::BTreeSet<Vec<u8>> = made
        .iter()
        .map(|n| std::fs::read(node.runner_dir(n).join("participant.key")).unwrap())
        .collect();
    assert_eq!(keys.len(), 3, "every runner has its own key");

    let fourth = RunnerJoin {
        name: Some("extra".into()),
        count: 1,
        ..join
    };
    let refused = runner_join(&node, fourth)
        .await
        .expect_err("the code is used up after three joins");
    assert!(refused.contains("join_code_unknown"), "{refused}");
    assert!(!node.runner_dir("extra").join("broker.toml").exists());

    let three = start_runners(&node, Some("heavy"), 3, None, false)
        .await
        .expect("the three runners start");
    for n in &made {
        runner_online(&main, n).await;
    }
    let listed = names(&main, AdminRequest::RunnerList).await;
    assert_eq!(listed.len(), 3, "{listed:?}");

    three.stop();
    roles.stop();
    remove(dir).await;
}

async fn kit_code(layout: &Layout, runners: u32) -> (String, String) {
    crate::kit::join_code(layout, None, None, Some(runners)).await
}
