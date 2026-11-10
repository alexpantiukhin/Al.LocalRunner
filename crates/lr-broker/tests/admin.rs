mod common;

use common::harness::*;
use lr_proto::BrokerToRunner;

#[tokio::test]
async fn reload_with_new_grace_sends_settings_update_to_runners() {
    let h = Harness::start("");
    let mut runner = h.local_runner("alex", "r1", linux()).await;
    let text = policy("[grace]\nlost_min = 7\n");
    let applied = h.handle.reload_policy(&text).await.expect("reload applies");
    assert_eq!(
        applied.settings_update.and_then(|u| u.broker_lost_kill_s),
        Some(420)
    );
    loop {
        match runner.runner().await {
            BrokerToRunner::SettingsUpdate(u) => {
                assert_eq!(u.broker_lost_kill_s, Some(420));
                break;
            }
            BrokerToRunner::Ack(_) => continue,
            other => panic!("expected settings_update, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn invalid_reload_is_refused_and_nothing_is_sent() {
    let h = Harness::start("");
    let mut runner = h.local_runner("alex", "r1", linux()).await;
    let err = h
        .handle
        .reload_policy("[grace]\nlost_min = 0\n")
        .await
        .unwrap_err();
    assert_eq!(err.key.as_deref(), Some("lost_min"));
    runner.quiet().await;
}

#[tokio::test]
async fn revoked_machine_loses_its_live_connection() {
    let h = Harness::start("");
    let mut runner = h.member_runner("box", "10.0.0.5", linux()).await;
    h.handle.revoked("box").await;
    assert!(runner.closed().await, "the revoked runner stays connected");
}
