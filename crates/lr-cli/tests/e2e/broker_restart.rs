use crate::common::hub_events::HubEvents;
use crate::common::TempDir;
use crate::support::*;
use lr_cli::doctor::Status;
use lr_cli::link_report;
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_core::JobState;
use lr_proto::QueueEvent;

async fn broker_gone(layout: &Layout) {
    until_hub(layout, "the hub to see the broker gone", || async {
        broker_available(layout).await.map_or(Some(()), |_| None)
    })
    .await;
}

async fn back_online(mut events: HubEvents, layout: &Layout, name: &str) {
    let online = async {
        loop {
            let event = events.next().await;
            if matches!(&event.event, QueueEvent::RunnerOnline { name: n } if n.ends_with(name)) {
                return;
            }
            let live = async {
                broker_available(layout).await?;
                runners(layout)
                    .await?
                    .into_iter()
                    .find(|r| r.online && r.name.ends_with(name))
            };
            if live.await.is_some() {
                return;
            }
        }
    };
    let online = tokio::time::timeout(HANG, online).await;
    events.close().await;
    online.unwrap_or_else(|_| panic!("timed out waiting for runner {name} on the new broker"));
}

async fn restart_and_run(dir: &TempDir, move_listen: bool) {
    let layout = layout(dir, "machine", "");
    add_runner(&layout, "r1");
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    runner_online(&layout, "r1").await;

    broker.stopped().await;
    broker_gone(&layout).await;

    if move_listen {
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = free.local_addr().unwrap().to_string();
        drop(free);
        let policy = std::fs::read_to_string(layout.config_file("policy.toml"))
            .unwrap()
            .replace("127.0.0.1:0", &address);
        std::fs::write(layout.config_file("policy.toml"), policy).unwrap();
    }

    let events = HubEvents::open(&layout).await;
    let broker = start_broker(&layout, false)
        .await
        .expect("the broker starts again");
    back_online(events, &layout, "r1").await;

    let profiles = "[profile.unit]\ncommand = \"echo reconnected\"\ntimeout_s = 60\nmem_mb = 64\n";
    let repo = repo(dir, profiles);
    let (client, job) = submit(&layout, &repo, "unit", None).await;
    let ended = until_exit(client, &job, HANG).await;
    assert_eq!(ended.state, JobState::Passed, "{ended:?}");

    runners.stopped().await;
    hub_role.stopped().await;
    broker.stopped().await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn up_and_doctor_name_a_runner_that_is_alive_but_not_connected() {
    let dir = TempDir::new("e2e-runner-link");
    let layout = layout(&dir, "machine", "");
    add_runner(&layout, "r1");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts without a broker");
    let line = link_report::runner_line(&layout, "r1")
        .await
        .expect("the runner reports it is not connected");
    assert!(
        line.contains("alive but not connected to the broker"),
        "{line}"
    );
    let checks = link_report::checks(&layout).await;
    assert!(
        checks
            .iter()
            .any(|c| c.name == "runner r1 link" && c.status == Status::Warn),
        "{checks:?}"
    );

    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    runner_online(&layout, "r1").await;
    until_hub(&layout, "the runner link line to go", || async {
        link_report::runner_line(&layout, "r1")
            .await
            .map_or(Some(()), |_| None)
    })
    .await;
    let checks = link_report::checks(&layout).await;
    assert!(
        checks.iter().all(|c| c.name != "runner r1 link"),
        "{checks:?}"
    );

    runners.stopped().await;
    hub_role.stopped().await;
    broker.stopped().await;
    remove(dir).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_lines_name_a_broker_that_went_away() {
    let dir = TempDir::new("e2e-hub-link");
    let layout = layout(&dir, "machine", "");
    add_runner(&layout, "r1");
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    runner_online(&layout, "r1").await;
    assert!(link_report::hub_lines(&layout).await.is_empty());

    broker.stopped().await;
    broker_gone(&layout).await;
    let lines = link_report::hub_lines(&layout).await;
    assert!(
        lines
            .iter()
            .any(|l| l.contains("alive but not connected to the broker")),
        "{lines:?}"
    );

    let broker = start_broker(&layout, false)
        .await
        .expect("the broker starts again");
    until_hub(&layout, "the hub to reconnect", || {
        broker_available(&layout)
    })
    .await;
    assert!(link_report::hub_lines(&layout).await.is_empty());

    runners.stopped().await;
    hub_role.stopped().await;
    broker.stopped().await;
    remove(dir).await;
}
