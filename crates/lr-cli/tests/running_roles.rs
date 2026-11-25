mod common;

use common::hub_events::HANG;
use common::TempDir;
use lr_cli::commands::up_cmd::{running_roles, selection, up_plan, up_selected, up_with};
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

async fn until_reported(what: &str, role: &Running, name: &'static str, label: &'static str) {
    let registries = role.metrics();
    assert_eq!(registries.len(), 1, "one logger");
    let metrics = registries[0].clone();
    let seen = tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + HANG;
        loop {
            let updates = metrics.updates();
            let found = metrics
                .snapshot()
                .iter()
                .any(|p| p.name == name && p.labels.iter().any(|(k, v)| k == "role" && v == label));
            if found {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) else {
                return false;
            };
            metrics.wait_updates(updates, left);
        }
    });
    assert!(seen.await.unwrap(), "timed out waiting for {what}");
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

    until_reported(
        "the hub's outbox gauge",
        &hub,
        "localrunner_outbox_pending",
        "hub",
    )
    .await;
    until_reported(
        "the runner's outbox gauge",
        &runners,
        "localrunner_outbox_pending",
        "runner",
    )
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
    up_failing(layout, role, within, &[]).await
}

async fn up_failing(
    layout: &Layout,
    role: RoleArg,
    within: Duration,
    fails: &[&str],
) -> (i32, Duration, Vec<String>) {
    let started = std::sync::Mutex::new(Vec::new());
    let spawn = |argv: &[String]| -> std::io::Result<u32> {
        started.lock().unwrap().push(argv[..2].join(" "));
        match fails.contains(&argv[0].as_str()) {
            true => Err(std::io::Error::other("cannot start")),
            false => Ok(4242),
        }
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

#[tokio::test(start_paused = true)]
async fn up_leaves_the_hub_down_without_waiting_when_the_broker_could_not_start() {
    let dir = TempDir::new("up-broker-failed");
    let layout = layout(&dir);
    let within = Duration::from_secs(6);
    let (code, took, tried) = up_failing(&layout, RoleArg::All, within, &["broker"]).await;
    assert_eq!(code, 1);
    assert_eq!(
        tried,
        vec!["broker run", "runner run"],
        "the hub is not started"
    );
    assert!(
        took >= within && took < within + Duration::from_secs(1),
        "only the started runner is waited for: {took:?}"
    );
    let (code, took, tried) =
        up_failing(&layout, RoleArg::All, within, &["broker", "runner"]).await;
    assert_eq!(code, 1);
    assert_eq!(tried, vec!["broker run", "runner run"]);
    assert!(
        took < Duration::from_secs(1),
        "nothing is waited for: {took:?}"
    );
}

fn with_second_runner(layout: &Layout) {
    std::fs::create_dir_all(layout.root.join("runner").join("r2")).unwrap();
}

#[test]
fn up_takes_the_names_of_down_and_lists_the_known_ones_for_an_unknown_name() {
    let dir = TempDir::new("up-names");
    let layout = layout(&dir);
    with_second_runner(&layout);
    let one = selection(&layout, &["r2".to_string()]).unwrap();
    assert_eq!(one.roles, vec![NodeRole::Runner]);
    assert_eq!(one.runners, Some(BTreeSet::from(["r2".to_string()])));
    let local = selection(&layout, &["local:r1".to_string(), "hub".to_string()]).unwrap();
    assert_eq!(local.roles, vec![NodeRole::Runner, NodeRole::Hub]);
    assert_eq!(local.runners, Some(BTreeSet::from(["r1".to_string()])));
    let every = selection(&layout, &["runner".to_string(), "r1".to_string()]).unwrap();
    assert_eq!(every.runners, None);
    let broker = selection(&layout, &["broker".to_string()]).unwrap();
    assert_eq!(broker.roles, vec![NodeRole::Broker]);
    let err = selection(&layout, &["r3".to_string(), "r2".to_string()]).unwrap_err();
    assert!(err.contains("r3"), "{err}");
    assert!(
        err.ends_with("names of this installation: hub, broker, runner, r1, r2"),
        "{err}"
    );
}

#[tokio::test(start_paused = true)]
async fn up_with_a_runner_name_starts_that_runner_only() {
    let dir = TempDir::new("up-one-runner");
    let layout = layout(&dir);
    with_second_runner(&layout);
    let started = std::sync::Mutex::new(Vec::new());
    let spawn = |argv: &[String]| -> std::io::Result<u32> {
        started.lock().unwrap().push(argv[..4].join(" "));
        Ok(4242)
    };
    let select = selection(&layout, &["r2".to_string()]).unwrap();
    let code = up_selected(&layout, || Ok(select), Duration::from_secs(2), &spawn, true)
        .await
        .unwrap();
    assert_eq!(code, 1, "the started runner does not answer in the test");
    assert_eq!(started.into_inner().unwrap(), vec!["runner run --name r2"]);
}
