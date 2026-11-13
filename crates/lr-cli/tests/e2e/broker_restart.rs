use crate::common::TempDir;
use crate::support::*;
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_core::JobState;
use std::time::Duration;

async fn restart_and_run(dir: &TempDir, move_listen: bool) {
    let layout = layout(dir, "machine", "");
    add_runner(&layout, "r1");
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    runner_online(&layout, "r1").await;

    broker.stop();
    eventually(
        "the hub to see the broker gone",
        Duration::from_secs(60),
        || async { broker_available(&layout).await.map_or(Some(()), |_| None) },
    )
    .await;

    if move_listen {
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = free.local_addr().unwrap().to_string();
        drop(free);
        let policy = std::fs::read_to_string(layout.config_file("policy.toml"))
            .unwrap()
            .replace("127.0.0.1:0", &address);
        std::fs::write(layout.config_file("policy.toml"), policy).unwrap();
    }

    let broker = start_broker(&layout, false)
        .await
        .expect("the broker starts again");
    eventually("the hub to reconnect", Duration::from_secs(90), || {
        broker_available(&layout)
    })
    .await;
    runner_online(&layout, "r1").await;

    let profiles = "[profile.unit]\ncommand = \"echo reconnected\"\ntimeout_s = 60\nmem_mb = 64\n";
    let repo = repo(dir, profiles);
    let (client, job) = submit(&layout, &repo, "unit", None).await;
    let ended = until_exit(client, &job, Duration::from_secs(60)).await;
    assert_eq!(ended.state, JobState::Passed, "{ended:?}");

    runners.stop();
    hub_role.stop();
    broker.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_and_runner_reconnect_after_the_broker_restarts() {
    let dir = TempDir::new("e2e-reconnect");
    restart_and_run(&dir, false).await;
    remove(dir).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_and_runner_reconnect_after_a_listen_change_and_restart() {
    let dir = TempDir::new("e2e-reconnect-listen");
    restart_and_run(&dir, true).await;
    remove(dir).await;
}
