mod common;

use common::broker::{OnOrder, Script};
use common::*;
use lr_core::JobState;
use lr_proto::{CliReply, CliRequest, HubOrder};

async fn orders(cli: &Cli) -> Vec<HubOrder> {
    cli.send(CliRequest::Orders).await;
    match cli.reply().await {
        CliReply::Orders { orders, .. } => orders,
        other => panic!("expected orders, got {other:?}"),
    }
}

#[tokio::test]
async fn an_order_no_broker_accepted_is_listed_as_outbox_with_its_log_copy() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner().on_order(OnOrder::Silent));
    rig.start();
    let cli = rig.cli().await;
    let job = cli.submit(submit("unit", None)).await;
    eventually("order sent", || local.orders().len() == 1).await;
    let list = orders(&rig.cli().await).await;
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0].summary.job, job);
    assert_eq!(list[0].summary.state, JobState::Queued);
    assert_eq!(list[0].summary.label.as_deref(), Some("agent"));
    assert_eq!(list[0].summary.project.as_deref(), Some("acme/app"));
    assert_eq!(list[0].broker.as_deref(), Some("local"));
    assert!(list[0].outbox);
    assert_eq!(
        list[0].log.as_deref(),
        Some(log_path(&job).display().to_string().as_str())
    );
}

#[tokio::test]
async fn an_accepted_order_is_listed_outside_the_outbox_and_only_to_its_os_user() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    let job = cli.submit(submit("unit", None)).await;
    eventually("accepted", || local.orders().len() == 1).await;
    settle().await;
    local.started(&job).await;
    settle().await;
    let list = orders(&rig.cli().await).await;
    assert_eq!(list.len(), 1, "{list:?}");
    assert!(!list[0].outbox);
    assert_eq!(list[0].summary.state, JobState::Running);
    let bob = Cli::open(&rig.net, "bob").await;
    assert_eq!(orders(&bob).await, Vec::<HubOrder>::new());
}

#[tokio::test]
async fn jobs_mine_shows_only_jobs_of_the_callers_os_user() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let job = rig.cli().await.submit(submit("unit", None)).await;
    eventually("accepted", || local.orders().len() == 1).await;
    settle().await;
    let mine = CliRequest::Jobs {
        label: "agent".into(),
        active: true,
    };
    let alex = rig.cli().await;
    alex.send(mine.clone()).await;
    match alex.reply().await {
        CliReply::Jobs { jobs } => {
            let ids: Vec<_> = jobs.iter().map(|j| j.job.clone()).collect();
            assert_eq!(ids, [job]);
        }
        other => panic!("{other:?}"),
    }
    let bob = Cli::open(&rig.net, "bob").await;
    bob.send(mine).await;
    assert_eq!(
        bob.reply().await,
        CliReply::Jobs { jobs: vec![] },
        "the same label of another OS user is not the caller"
    );
}
