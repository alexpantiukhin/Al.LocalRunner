mod common;

use common::harness::*;
use lr_core::JobState;
use lr_proto::*;

const ALLOW: &str =
    "[[secrets.allow]]\nlabel = \"ci\"\nnames = [\"registry_auth\", \"signing_key\"]\n\
[[secrets.allow]]\nlabel = \"agent\"\nnames = [\"registry_auth\"]\n";

fn with_secrets(names: &[&str]) -> lr_core::Requirements {
    let mut r = req(512, 1, 60);
    r.secrets = names.iter().map(|s| s.to_string()).collect();
    r
}

#[tokio::test]
async fn secret_not_allowed_for_label_rejected() {
    let h = Harness::start(ALLOW);
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.member_hub("laptop", "agent", "10.0.0.20").await;
    hub.send(&order(
        "j1",
        with_secrets(&["registry_auth", "signing_key"]),
    ))
    .await;
    let r = hub.rejected().await;
    assert_eq!(r.code.as_str(), "rejected_secret_not_allowed");
    assert_eq!(r.detail, "секрет signing_key не разрешён ярлыку agent");
    assert!(h.job_row("laptop", "j1").await.is_none());
}

#[tokio::test]
async fn allowed_secret_names_travel_in_the_assignment() {
    let h = Harness::start(ALLOW);
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.member_hub("ci-box", "ci", "10.0.0.21").await;
    hub.send(&order(
        "j1",
        with_secrets(&["signing_key", "registry_auth"]),
    ))
    .await;
    hub.accepted().await;
    assert_eq!(
        r.assign().await.spec.secrets,
        ["registry_auth", "signing_key"]
    );
}

#[tokio::test]
async fn no_label_may_request_secrets_without_an_allow_entry() {
    let h = Harness::start("");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.member_hub("ci-box", "ci", "10.0.0.21").await;
    hub.send(&order("j1", with_secrets(&["registry_auth"])))
        .await;
    let r = hub.rejected().await;
    assert_eq!(r.code.as_str(), "rejected_secret_not_allowed");
    assert_eq!(r.detail, "секрет registry_auth не разрешён ярлыку ci");
}

#[tokio::test]
async fn secret_missing_on_the_runner_ends_the_job_with_its_reason() {
    let h = Harness::start(ALLOW);
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.member_hub("ci-box", "ci", "10.0.0.21").await;
    hub.send(&order("j1", with_secrets(&["registry_auth"])))
        .await;
    hub.accepted().await;
    r.assign().await;
    r.send(&RunnerToBroker::JobResult(JobResult {
        job: lr_core::JobId("j1".into()),
        seq: 1,
        state: JobState::Failed,
        exit_code: None,
        reason: Some(ReasonCode::SecretMissing),
        detail: Some("секрет не найден: registry_auth".into()),
        peak_disk_mb: None,
        peak_mem_mb: None,
    }))
    .await;
    r.acked(1).await;
    let row = h.until_state("ci-box", "j1", JobState::Failed).await;
    assert_eq!(
        row.job.cancel_reason.as_deref(),
        Some("secret_missing: секрет не найден: registry_auth")
    );
    let result = loop {
        if let BrokerToHub::JobResult(res) = hub.hub().await {
            break res;
        }
    };
    assert_eq!(result.reason, Some(ReasonCode::SecretMissing));
}
