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
    assert_eq!(
        r.detail,
        "secret signing_key is not allowed for label agent; to allow it: localrunner policy allow-secret signing_key --label agent"
    );
    h.assert_recorded_rejection("laptop", "j1", "rejected_secret_not_allowed")
        .await;
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
    assert_eq!(
        r.detail,
        "secret registry_auth is not allowed for label ci; to allow it: localrunner policy allow-secret registry_auth --label ci"
    );
}

#[tokio::test]
async fn not_allowed_secret_names_the_file_the_label_and_the_fix() {
    let path = "/home/alex/.config/localrunner/policy.toml";
    let h = Harness::with_policy_path(ALLOW, path, false);
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.member_hub("laptop", "agent", "10.0.0.20").await;
    hub.send(&order("j1", with_secrets(&["signing_key"]))).await;
    let r = hub.rejected().await;
    assert_eq!(r.code.as_str(), "rejected_secret_not_allowed");
    assert_eq!(
        r.detail,
        format!("secret signing_key is not allowed for label agent in {path}; to allow it: localrunner policy allow-secret signing_key --label agent")
    );
}

#[tokio::test]
async fn not_allowed_secret_of_the_service_file_says_to_run_the_fix_as_administrator() {
    let path = "C:\\ProgramData\\Al.LocalRunner\\policy.toml";
    let h = Harness::with_policy_path("", path, true);
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.member_hub("blank", "", "10.0.0.20").await;
    hub.send(&order("j1", with_secrets(&["registry_auth"])))
        .await;
    let r = hub.rejected().await;
    assert_eq!(r.code.as_str(), "rejected_secret_not_allowed");
    assert_eq!(
        r.detail,
        format!("secret registry_auth is not allowed for label (empty) in {path}; to allow it, run it as administrator: localrunner policy allow-secret registry_auth --label ''")
    );
}

#[tokio::test]
async fn empty_label_matches_only_clients_without_a_label() {
    let h = Harness::start("[[secrets.allow]]\nlabel = \"\"\nnames = [\"registry_auth\"]\n");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut local = h.local_hub("root").await;
    local
        .send(&order("j1", with_secrets(&["registry_auth"])))
        .await;
    local.accepted().await;
    let mut blank = h.member_hub("blank", "", "10.0.0.20").await;
    blank
        .send(&order("j2", with_secrets(&["registry_auth"])))
        .await;
    blank.accepted().await;
    let mut ci = h.member_hub("ci-box", "ci", "10.0.0.21").await;
    ci.send(&order("j3", with_secrets(&["registry_auth"])))
        .await;
    assert_eq!(
        ci.rejected().await.code.as_str(),
        "rejected_secret_not_allowed"
    );
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
        detail: Some("secret not found: registry_auth".into()),
        peak_disk_mb: None,
        peak_mem_mb: None,
        finished_at: None,
    }))
    .await;
    r.acked(1).await;
    let row = h.until_state("ci-box", "j1", JobState::Failed).await;
    assert_eq!(
        row.job.cancel_reason.as_deref(),
        Some("secret_missing: secret not found: registry_auth")
    );
    let result = loop {
        if let BrokerToHub::JobResult(res) = hub.hub().await {
            break res;
        }
    };
    assert_eq!(result.reason, Some(ReasonCode::SecretMissing));
}

#[tokio::test]
async fn the_suggested_fix_quotes_what_a_shell_would_expand() {
    let h = Harness::start("");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    for (i, (label, name, words)) in [
        (
            "team$HOME",
            "registry_auth",
            "registry_auth --label 'team$HOME'",
        ),
        (
            "my team",
            "registry_auth",
            "registry_auth --label 'my team'",
        ),
        ("", "registry_auth", "registry_auth --label ''"),
        ("ci", "db$x", "'db$x' --label ci"),
        ("it's", "a b", "'a b' --label 'it''s'"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut hub = h
            .member_hub(&format!("hub{i}"), label, &format!("10.0.0.{}", 20 + i))
            .await;
        hub.send(&order("j1", with_secrets(&[name]))).await;
        let r = hub.rejected().await;
        assert_eq!(r.code.as_str(), "rejected_secret_not_allowed");
        assert!(
            r.detail
                .ends_with(&format!("localrunner policy allow-secret {words}")),
            "{}",
            r.detail
        );
    }
}
