mod common;

use common::TempDir;
use lr_cli::commands::jobs::hub;
use lr_cli::commands::view::{known_brokers, read};
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_proto::{ReadReply, ReadRequest};
use std::collections::BTreeSet;
use std::time::Duration;

fn runner_toml(layout: &Layout, programs: &str) {
    let dir = layout.root.join("runner").join("r1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("runner.toml"),
        format!(
            "allow_elevated = true\nmem_mb = 1024\ncpu = 1\n\n[programs]\n{programs}\n{}",
            common::env_path_toml()
        ),
    )
    .unwrap();
}

fn layout(dir: &TempDir) -> Layout {
    let root = dir.path().join("state");
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    let user = lr_net::current_os_user().unwrap();
    let policy = format!("[listen]\naddress = []\n\n[[local_user]]\nname = {user:?}\n");
    std::fs::write(config.join("policy.toml"), policy).unwrap();
    Layout::new(root, config)
}

async fn offered(layout: &Layout, expected: &BTreeSet<String>) -> BTreeSet<String> {
    let mut last = BTreeSet::new();
    for _ in 0..150 {
        if let Ok(client) = hub(layout).await {
            let brokers = known_brokers(&client).await.unwrap_or_default();
            if let Some(b) = brokers.into_iter().find(|b| b.available) {
                if let Ok(ReadReply::Runners { runners }) =
                    read(&client, &b.name, ReadRequest::Runners).await
                {
                    if let Some(r) = runners
                        .into_iter()
                        .find(|r| r.online && r.name.ends_with("r1"))
                    {
                        last = r.caps.programs.clone();
                        if &last == expected {
                            client.close().await;
                            return last;
                        }
                    }
                }
            }
            client.close().await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    last
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn programs_are_checked_again_on_every_start_of_the_runner() {
    let dir = TempDir::new("programs-up");
    let layout = layout(&dir);
    let check = if cfg!(windows) { "exit 0" } else { "true" };
    runner_toml(
        &layout,
        &format!("git = {{ check = \"{check}\" }}\nsurely-missing-program-xyz = {{}}\n"),
    );
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");

    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    let only_git = BTreeSet::from(["git".to_string()]);
    assert_eq!(offered(&layout, &only_git).await, only_git);
    runners.stop();

    runner_toml(&layout, "git = { check = \"exit 7\" }\n");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts again");
    let none = BTreeSet::new();
    assert_eq!(
        offered(&layout, &none).await,
        none,
        "the failing check of the second start is applied"
    );
    runners.stop();
    hub_role.stop();
    broker.stop();
}
