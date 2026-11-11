mod common;

use common::loopback::{layout, place, repo, until_ready, until_state};
use common::TempDir;
use lr_abstractions::{CancelRequest, DataSource};
use lr_cli::dashboard::HubSource;
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_core::JobState;
use lr_proto::ReasonCode;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_from_the_dashboard_reaches_the_broker_forced_with_its_actor_and_reason() {
    let dir = TempDir::new("dashboard-cancel");
    let layout = layout(&dir);
    let repo = repo(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    until_ready(&layout).await;

    let other = common::session("someone-else", None, &repo).unwrap();
    let running = place(&layout, other.clone()).await;
    let queued = place(&layout, other).await;
    until_state(&layout, &running, JobState::Running).await;

    let source = HubSource::new(layout.clone());
    for job in [&running, &queued] {
        source
            .cancel(
                job,
                CancelRequest {
                    actor: "dashboard:alex".into(),
                    reason: Some("stuck on the emulator".into()),
                },
            )
            .await
            .expect("the dashboard cancels a job of another label");
    }

    for job in [&queued, &running] {
        let d = until_state(&layout, job, JobState::Cancelled).await;
        assert_eq!(d.summary.reason, Some(ReasonCode::CancelledByUser), "{job}");
        assert_eq!(d.cancelled_by.as_deref(), Some("dashboard:alex"), "{job}");
        let copy = repo.join("temp").join("runner").join(format!("{job}.log"));
        let text = std::fs::read_to_string(&copy).unwrap();
        assert!(
            text.contains("cancel requested by dashboard:alex")
                && text.contains("--force: stuck on the emulator"),
            "{text}"
        );
    }

    runners.stop();
    hub_role.stop();
    broker.stop();
}
