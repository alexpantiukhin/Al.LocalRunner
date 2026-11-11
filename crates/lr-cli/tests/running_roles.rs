mod common;

use common::TempDir;
use lr_cli::commands::up_cmd::{running_roles, up_plan, up_with};
use lr_cli::commands::RoleArg;
use lr_cli::init::NodeRole;
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, start_runners, Running};
use std::collections::BTreeSet;
use std::time::Duration;

fn layout(dir: &TempDir) -> Layout {
    let root = dir.path().join("state");
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    let user = lr_net::current_os_user().unwrap();
    let policy = format!("[listen]\naddress = []\n\n[[local_user]]\nname = {user:?}\n");
    std::fs::write(config.join("policy.toml"), policy).unwrap();
    let runner = root.join("runner").join("r1");
    std::fs::create_dir_all(&runner).unwrap();
    std::fs::write(
        runner.join("runner.toml"),
        format!(
            "allow_elevated = true\nmem_mb = 1024\ncpu = 1\n{}",
            common::env_path_toml()
        ),
    )
    .unwrap();
    Layout::new(root, config)
}

fn reports(role: &Running, name: &str, label: &str) -> bool {
    role.metrics().iter().any(|m| {
        m.snapshot()
            .iter()
            .any(|p| p.name == name && p.labels.iter().any(|(k, v)| k == "role" && v == label))
    })
}

async fn until(what: &str, probe: impl Fn() -> bool) {
    for _ in 0..150 {
        if probe() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_up_starts_nothing_and_hub_and_runner_report_into_their_logger_registry() {
    let dir = TempDir::new("up-twice");
    let layout = layout(&dir);
    let all = NodeRole::ALL.to_vec();
    let before = running_roles(&layout).await;
    assert_eq!(up_plan(&layout, &all, &before).len(), 3);

    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub = start_hub(&layout, false).await.expect("hub starts");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");

    let now = running_roles(&layout).await;
    assert!(now.broker && now.hub, "{now:?}");
    assert_eq!(now.runners, BTreeSet::from(["r1".to_string()]));
    assert!(
        up_plan(&layout, &all, &now).is_empty(),
        "{:?}",
        up_plan(&layout, &all, &now)
    );

    until("the hub's outbox gauge", || {
        reports(&hub, "localrunner_outbox_pending", "hub")
    })
    .await;
    until("the runner's outbox gauge", || {
        reports(&runners, "localrunner_outbox_pending", "runner")
    })
    .await;
    assert!(hub.metrics().iter().all(|m| m.rejected() == 0));
    assert!(runners.metrics().iter().all(|m| m.rejected() == 0));

    runners.stop();
    hub.stop();
    broker.stop();
}

async fn up_timed(
    layout: &Layout,
    role: RoleArg,
    within: Duration,
) -> (i32, Duration, Vec<String>) {
    let started = std::sync::Mutex::new(Vec::new());
    let spawn = |argv: &[String]| -> std::io::Result<u32> {
        started.lock().unwrap().push(argv[..2].join(" "));
        Ok(4242)
    };
    let t0 = tokio::time::Instant::now();
    let code = up_with(layout, Some(role), within, &spawn).await.unwrap();
    let took = t0.elapsed();
    (code, took, started.into_inner().unwrap())
}

#[tokio::test(start_paused = true)]
async fn up_leaves_the_hub_down_once_the_broker_is_silent_for_its_limit_and_waits_only_once() {
    let dir = TempDir::new("up-silent-broker");
    let layout = layout(&dir);
    let within = Duration::from_secs(7);
    let (code, took, started) = up_timed(&layout, RoleArg::All, within).await;
    assert_eq!(code, 1);
    assert!(took >= within, "not before the limit: {took:?}");
    assert!(
        took < within + Duration::from_secs(1),
        "the roles are not waited for a second time: {took:?}"
    );
    assert_eq!(
        started,
        vec!["broker run", "runner run"],
        "the hub is not started"
    );
}

#[tokio::test(start_paused = true)]
async fn up_reports_a_silent_role_after_its_limit_and_not_before() {
    let dir = TempDir::new("up-silent-runner");
    let layout = layout(&dir);
    let within = Duration::from_secs(4);
    let (code, took, started) = up_timed(&layout, RoleArg::Runner, within).await;
    assert_eq!(started, vec!["runner run"]);
    assert_eq!(code, 1);
    assert!(took >= within, "not before the limit: {took:?}");
    assert!(took < within + Duration::from_secs(1), "{took:?}");
}
