mod common;

use common::broker::Script;
use common::*;
use lr_core::{JobId, JobState, Timestamp};
use lr_proto::{
    BrokerToHub, CliReply, CliRequest, DashboardEvent, EventMsg, HubToBroker, JobSummary,
    QueueEvent, ReadRequest, Topic,
};

fn queued(job: &str) -> QueueEvent {
    QueueEvent::JobQueued {
        job: Box::new(JobSummary {
            job: JobId(job.into()),
            client_label: "bob".into(),
            state: JobState::Queued,
            platform: None,
            runner: None,
            queued_at: Timestamp(1),
            started_at: None,
            finished_at: None,
            exit_code: None,
            reason: None,
            project: None,
            label: None,
            profile: None,
            source: None,
            note: None,
            branch: None,
            cwd: None,
            timeout_s: None,
            github_run_url: None,
            peak_mem_mb: None,
            peak_disk_mb: None,
            labels: Vec::new(),
        }),
    }
}

#[tokio::test]
async fn the_hub_subscribes_to_its_brokers_and_relays_their_events_to_a_watching_cli() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    let _hub = rig.start();
    eventually("the hub subscribes to the broker's events", || {
        local.received().iter().any(|m| {
            matches!(m, HubToBroker::Read(c) if matches!(&c.request,
                ReadRequest::Subscribe { topics } if topics.contains(&Topic::Queue)
                    && topics.contains(&Topic::Runners)))
        })
    })
    .await;

    let watcher = rig.cli().await;
    watcher.send(CliRequest::Events).await;
    assert_eq!(watcher.reply().await, CliReply::Watching);
    let other = rig.cli().await;

    local
        .send(BrokerToHub::Event(EventMsg {
            sub: 1,
            event: queued("j-masked"),
        }))
        .await;
    local
        .send(BrokerToHub::Event(EventMsg {
            sub: 1,
            event: QueueEvent::JobStarted {
                job: JobId("j-masked".into()),
                runner: "r1".into(),
            },
        }))
        .await;
    assert_eq!(
        watcher.reply().await,
        CliReply::Event(DashboardEvent {
            broker: "local".into(),
            event: queued("j-masked"),
        }),
        "the event comes as the broker masked it, nothing added"
    );
    assert_eq!(
        watcher.reply().await,
        CliReply::Event(DashboardEvent {
            broker: "local".into(),
            event: QueueEvent::JobStarted {
                job: JobId("j-masked".into()),
                runner: "r1".into(),
            },
        })
    );

    other.send(CliRequest::Brokers).await;
    assert!(
        matches!(other.reply().await, CliReply::Brokers { .. }),
        "a connection that did not ask for events gets none"
    );
}

fn reread_of(broker: &str) -> CliReply {
    CliReply::Event(DashboardEvent {
        broker: broker.into(),
        event: QueueEvent::Unknown,
    })
}

#[tokio::test]
async fn a_lost_and_regained_broker_connection_tells_the_watchers_to_reread() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    let _hub = rig.start();
    eventually("hub connects", || local.hellos() == 1).await;
    settle().await;
    let watcher = rig.cli().await;
    watcher.send(CliRequest::Events).await;
    assert_eq!(watcher.reply().await, CliReply::Watching);

    local.disconnect().await;
    assert_eq!(
        watcher.reply().await,
        reread_of("local"),
        "the connection went: what the broker sent meanwhile is lost"
    );
    settle().await;
    rig.step(1).await;
    eventually("redialed", || local.hellos() == 2).await;
    assert_eq!(
        watcher.reply().await,
        reread_of("local"),
        "the connection is back: reread the broker's state"
    );
}

#[tokio::test]
async fn a_repeated_events_request_opens_no_second_stream() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    let _hub = rig.start();
    eventually("hub connects", || local.hellos() == 1).await;
    settle().await;
    let watcher = rig.cli().await;
    watcher.send(CliRequest::Events).await;
    assert_eq!(watcher.reply().await, CliReply::Watching);
    watcher.send(CliRequest::Events).await;
    assert_eq!(watcher.reply().await, CliReply::Watching);
    settle().await;

    local
        .send(BrokerToHub::Event(EventMsg {
            sub: 1,
            event: started_event(),
        }))
        .await;
    local
        .send(BrokerToHub::Event(EventMsg {
            sub: 1,
            event: queued("j-next"),
        }))
        .await;
    assert!(
        matches!(watcher.reply().await, CliReply::Event(e) if e.event == started_event()),
        "the event comes once"
    );
    assert!(
        matches!(watcher.reply().await, CliReply::Event(e) if e.event == queued("j-next")),
        "the next one follows it, not a copy of the first"
    );
    settle().await;
    watcher.send(CliRequest::Brokers).await;
    let next = watcher.reply().await;
    assert!(
        matches!(next, CliReply::Brokers { .. }),
        "no copy of the stream is queued: {next:?}"
    );
}

fn started_event() -> QueueEvent {
    QueueEvent::JobStarted {
        job: JobId("j-once".into()),
        runner: "r1".into(),
    }
}
