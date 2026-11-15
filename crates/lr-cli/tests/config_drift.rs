mod common;

use common::loopback::{layout, until_ready};
use common::TempDir;
use lr_cli::commands::reload_cmd::reload_all;
use lr_cli::commands::RoleArg;
use lr_cli::config_drift::{disk_differs, drift_line, hash_text, lines};
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_proto::ControlReply;

fn link(config_hash: Option<String>) -> ControlReply {
    ControlReply::Link {
        connected: true,
        since: None,
        config_hash,
    }
}

#[test]
fn an_unchanged_file_is_not_reported_and_a_changed_one_is() {
    let hash = hash_text("tags = []\n");
    assert_eq!(hash.len(), 64);
    assert!(hash
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    assert!(!disk_differs("tags = []\n", &hash));
    assert!(disk_differs("tags = [\"x\"]\n", &hash));
    assert_eq!(
        drift_line("r1", "tags = []\n", &link(Some(hash.clone()))),
        None
    );
    assert_eq!(
        drift_line("r1", "tags = [\"x\"]\n", &link(Some(hash))).as_deref(),
        Some("runner r1 runs with an older runner.toml: localrunner reload")
    );
}

#[test]
fn a_role_that_does_not_report_a_hash_is_skipped_without_a_warning() {
    assert_eq!(drift_line("r1", "anything", &link(None)), None);
    assert_eq!(drift_line("r1", "anything", &ControlReply::Ok), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_changed_after_load_is_reported_until_reload() {
    let dir = TempDir::new("config-drift");
    let layout = layout(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub = start_hub(&layout, false).await.expect("hub starts");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    until_ready(&layout).await;
    assert!(lines(&layout).await.is_empty(), "nothing changed yet");

    let file = layout.runner_dir("r1").join("runner.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, format!("tags = [\"new\"]\n{text}")).unwrap();
    assert_eq!(
        lines(&layout).await,
        ["runner r1 runs with an older runner.toml: localrunner reload"]
    );

    reload_all(&layout, Some(RoleArg::Runner), None)
        .await
        .unwrap();
    assert!(lines(&layout).await.is_empty(), "reload clears the warning");

    runners.stop();
    hub.stop();
    broker.stop();
}
