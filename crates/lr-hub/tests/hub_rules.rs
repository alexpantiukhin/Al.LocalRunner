mod common;

use common::broker::{caps, Script};
use common::*;
use lr_core::{JobId, JobState, Zone};
use lr_hub::select::broker_zone;
use lr_hub::{BrokerStrategy, LOCAL_BROKER};
use lr_proto::{
    BrokerToHub, Chunk, CliReply, CliRequest, HubToBroker, ReasonCode, RejectReason, RunnerCaps,
};

async fn connected(brokers: &[&common::broker::FakeBroker]) {
    for b in brokers {
        eventually("broker connected", || b.resumed()).await;
    }
    settle().await;
}

fn two_runners(zone: Zone) -> Script {
    let mut s = Script::linux(zone);
    s.runners.push(RunnerCaps {
        name: "r2".into(),
        online: true,
        zone,
        caps: caps(&["linux"], None),
    });
    s
}

async fn least_loaded_tie(order: &[&str]) -> (usize, usize) {
    let mut rig = Rig::new();
    rig.policy.brokers.strategy = BrokerStrategy::LeastLoaded;
    rig.policy.brokers.order = order.iter().map(|s| s.to_string()).collect();
    rig.join("lan-a").await;
    rig.join("lan-b").await;
    let a = rig.broker("lan-a", Zone::Lan, two_runners(Zone::Lan));
    let b = rig.broker("lan-b", Zone::Lan, two_runners(Zone::Lan));
    rig.start();
    connected(&[&a, &b]).await;
    rig.cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    eventually("order placed", || a.orders().len() + b.orders().len() == 1).await;
    settle().await;
    (a.orders().len(), b.orders().len())
}

#[tokio::test]
async fn least_loaded_breaks_a_tie_by_the_brokers_order() {
    assert_eq!(least_loaded_tie(&["lan-b", "lan-a"]).await, (0, 1));
    assert_eq!(least_loaded_tie(&["lan-a", "lan-b"]).await, (1, 0));
}

#[tokio::test]
async fn a_broker_zone_is_remote_until_reported_and_the_local_broker_is_always_local() {
    assert_eq!(broker_zone("far", None), Zone::Remote);
    assert_eq!(broker_zone("far", Some(Zone::Lan)), Zone::Lan);
    assert_eq!(broker_zone(LOCAL_BROKER, Some(Zone::Remote)), Zone::Local);
    assert_eq!(broker_zone(LOCAL_BROKER, None), Zone::Local);
}

#[tokio::test]
async fn a_broker_that_reports_no_zone_never_takes_a_lan_limited_order() {
    let mut rig = Rig::new();
    let profiles = PROFILES.replace(
        "platforms = [\"linux\"]",
        "platforms = [\"linux\"]\nzone = \"lan\"",
    );
    rig.repo
        .commit_file(COMMIT, ".localrunner/profiles.toml", profiles.as_bytes());
    rig.files.put(
        &repo_root().join(".localrunner").join("profiles.toml"),
        profiles.as_bytes(),
    );
    rig.join("quiet").await;
    let quiet = rig.broker("quiet", Zone::Lan, Script::linux(Zone::Lan));
    quiet.report_zone(None);
    rig.start();
    connected(&[&quiet]).await;
    let cli = rig.cli().await;
    let mut req = submit("remote", Some("linux"));
    req.wait = true;
    cli.submit(req).await;
    match cli.reply().await {
        CliReply::Rejected { code, .. } => {
            assert_eq!(code, ReasonCode::Rejected(RejectReason::ZoneNotServed))
        }
        other => panic!("{other:?}"),
    }
    assert!(quiet.orders().is_empty());
}

#[tokio::test]
async fn done_has_no_reply_and_the_reaper_still_follows_the_sender() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    let job = cli.submit(submit("unit", None)).await;
    eventually("accepted", || local.orders().len() == 1).await;
    cli.send(CliRequest::Done { job: job.clone() }).await;
    assert!(cli.recv_frame().await.is_none(), "done has no answer");
    cli.close().await;
    settle().await;
    rig.step(10).await;
    assert!(local.cancels().is_empty(), "the sender is alive");
    assert!(rig.active(&job).await.is_some());

    rig.inspector.remove(SENDER_PID);
    rig.step(12).await;
    eventually("cancelled after done", || local.cancels().len() == 1).await;
    assert_eq!(local.cancels()[0].job, job);
    assert_eq!(local.cancels()[0].code, ReasonCode::SenderGone);
}

#[tokio::test]
async fn a_message_from_a_broker_not_holding_the_order_is_acked_and_not_applied() {
    let mut rig = Rig::new();
    rig.policy.brokers.order = vec!["lan-a".into(), "lan-b".into()];
    rig.join("lan-a").await;
    rig.join("lan-b").await;
    let a = rig.broker("lan-a", Zone::Lan, Script::linux(Zone::Lan));
    let b = rig.broker("lan-b", Zone::Lan, Script::linux(Zone::Lan));
    rig.start();
    connected(&[&a, &b]).await;
    let job = rig
        .cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    eventually("accepted by a", || a.orders().len() == 1).await;
    settle().await;
    b.send(BrokerToHub::Chunk(Chunk {
        job: job.clone(),
        seq: 1,
        step: Some(0),
        text: "foreign line\n".into(),
    }))
    .await;
    b.result(&job, JobState::Failed, Some(1)).await;
    settle().await;
    let acked: Vec<u64> = b
        .acks()
        .into_iter()
        .filter(|(j, _)| j == &job)
        .map(|(_, s)| s)
        .collect();
    assert_eq!(acked.len(), 2, "{acked:?}");
    let active = rig.active(&job).await.expect("still active");
    assert_eq!(active.last_seq, 0);
    let log = rig
        .store
        .file(&log_path(&job))
        .map(|b| String::from_utf8(b).unwrap())
        .unwrap_or_default();
    assert!(!log.contains("foreign line"), "{log}");
    a.started(&job).await;
    settle().await;
    assert_eq!(rig.active(&job).await.unwrap().last_seq, 1);
    assert!(a
        .received()
        .iter()
        .all(|m| !matches!(m, HubToBroker::Cancel(_))));
}

#[tokio::test]
async fn jobs_without_active_also_lists_finished_results_kept_in_memory() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    let done = cli.submit(submit("unit", None)).await;
    let running = cli.submit(submit("unit", None)).await;
    eventually("accepted", || local.orders().len() == 2).await;
    settle().await;
    local.started(&done).await;
    local.result(&done, JobState::Passed, Some(0)).await;
    local.started(&running).await;
    settle().await;
    let jobs = |active: bool| CliRequest::Jobs {
        label: "agent".into(),
        active,
    };
    let listed = |reply: CliReply| -> Vec<(JobId, JobState)> {
        match reply {
            CliReply::Jobs { jobs } => {
                let mut v: Vec<_> = jobs.into_iter().map(|j| (j.job, j.state)).collect();
                v.sort_by(|x, y| x.0.cmp(&y.0));
                v
            }
            other => panic!("{other:?}"),
        }
    };
    cli.send(jobs(true)).await;
    assert_eq!(
        listed(cli.reply().await),
        vec![(running.clone(), JobState::Running)]
    );
    cli.send(jobs(false)).await;
    let mut want = vec![
        (done.clone(), JobState::Passed),
        (running.clone(), JobState::Running),
    ];
    want.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(listed(cli.reply().await), want);
    let bob = Cli::open(&rig.net, "bob").await;
    for active in [true, false] {
        bob.send(jobs(active)).await;
        assert_eq!(
            listed(bob.reply().await),
            Vec::new(),
            "another OS user with the same label sees none (active = {active})"
        );
    }
}
