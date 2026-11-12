mod common;

use common::TempDir;
use lr_cli::commands::logging_cmd::{logging_reload, reload_all, Reload};
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, start_runners};

fn layout(dir: &TempDir) -> Layout {
    let root = dir.path().join("state");
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    let user = lr_net::current_os_user().unwrap();
    std::fs::write(
        config.join("policy.toml"),
        format!("[listen]\naddress = []\n\n[[local_user]]\nname = {user:?}\n"),
    )
    .unwrap();
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

fn all(answer: Reload) -> Vec<(String, Reload)> {
    ["r1", "hub", "broker"]
        .into_iter()
        .map(|r| (r.to_string(), answer.clone()))
        .collect()
}

fn refused(answers: &[(String, Reload)]) -> Vec<String> {
    answers
        .iter()
        .filter(|(_, a)| matches!(a, Reload::Refused(_)))
        .map(|(r, _)| r.clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn logging_reload_reaches_the_broker_the_hub_and_every_runner() {
    let dir = TempDir::new("logging-reload-all");
    let layout = layout(&dir);
    assert_eq!(reload_all(&layout).await.unwrap(), all(Reload::NotRunning));
    assert_eq!(logging_reload(&layout).await, Ok(1), "no role runs");

    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub = start_hub(&layout, false).await.expect("hub starts");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    let file = layout.config_file("logging.toml");

    assert_eq!(reload_all(&layout).await.unwrap(), all(Reload::Reloaded));
    assert_eq!(logging_reload(&layout).await, Ok(0));

    std::fs::write(&file, "level = = broken\n").unwrap();
    let answers = reload_all(&layout).await.unwrap();
    assert_eq!(refused(&answers), ["r1", "hub", "broker"], "{answers:?}");
    for (role, answer) in &answers {
        if let Reload::Refused(why) = answer {
            assert!(why.contains("logging.toml"), "{role}: {why}");
        }
    }
    assert_eq!(logging_reload(&layout).await, Ok(1));

    std::fs::write(&file, "[logging]\nlevel = \"debug\"\n").unwrap();
    assert_eq!(reload_all(&layout).await.unwrap(), all(Reload::Reloaded));

    runners.stop();
    assert_eq!(
        reload_all(&layout)
            .await
            .unwrap()
            .into_iter()
            .find(|(r, _)| r == "hub")
            .map(|(_, a)| a),
        Some(Reload::Reloaded)
    );
    hub.stop();
    broker.stop();
}
