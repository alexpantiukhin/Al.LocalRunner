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
