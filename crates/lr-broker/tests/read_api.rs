mod common;

use common::harness::*;
use lr_broker::BusEvent;
use lr_core::{JobId, JobState, Zone};
use lr_proto::*;

async fn pool_with_jobs(h: &Harness) -> (Conn, Conn) {
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&order("j1", req(1024, 2, 60))).await;
    alex.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    r.send(&chunk("j1", 2, "secret output\n")).await;
    r.acked(2).await;
    alex.send(&order("j2", req(512, 1, 60))).await;
    alex.accepted().await;
    alex.sync().await;
    (r, alex)
}

fn status(reply: ReadReply) -> StatusView {
    match reply {
        ReadReply::Status(s) => s,
        other => panic!("expected status, got {other:?}"),
    }
}

fn history(reply: ReadReply) -> Vec<JobSummary> {
    match reply {
        ReadReply::History { jobs, .. } => jobs,
        other => panic!("expected history, got {other:?}"),
    }
}

async fn log_of(c: &mut Conn, job: &str) -> Result<String, ReadReply> {
    c.send(&HubToBroker::Read(ReadCall {
        req: 900,
        request: ReadRequest::Log {
            job: JobId(job.into()),
            step: None,
            tail: None,
            follow: false,
            client: None,
        },
    }))
    .await;
    let mut text = String::new();
    loop {
        match c.hub().await {
            BrokerToHub::LogChunk(l) if l.req == 900 => {
                text.push_str(&l.text);
                if l.end {
                    return Ok(text);
                }
            }
            BrokerToHub::ReadReply(a) if a.req == 900 => return Err(a.reply),
            _ => continue,
        }
    }
}

fn show(job: &str) -> ReadRequest {
    ReadRequest::HistoryShow {
        job: JobId(job.into()),
        client: None,
    }
}

#[tokio::test]
async fn read_api_hides_others_orders_per_visibility() {
    let h = Harness::start("[visibility]\nothers_orders = \"none\"\n");
    let (_r, _alex) = pool_with_jobs(&h).await;
    let mut bob = h.local_hub("bob").await;
    let s = status(bob.read(ReadRequest::Status(StatusFilter::default())).await);
    assert!(s.running.is_empty() && s.queue.is_empty());
    assert_eq!(s.pool.used_cpu, 2);
    assert_eq!(s.pool.used_mem_mb, 1024);
    assert!(history(
        bob.read(ReadRequest::History(HistoryFilter::default()))
            .await
    )
    .is_empty());
    assert!(matches!(
        bob.read(show("j1")).await,
        ReadReply::NotFound { .. }
    ));

    let h = Harness::start("");
    let (_r, _alex) = pool_with_jobs(&h).await;
    let mut bob = h.local_hub("bob").await;
    let s = status(bob.read(ReadRequest::Status(StatusFilter::default())).await);
    let run = &s.running[0];
    assert_eq!(run.summary.job, JobId("j1".into()));
    assert_eq!(run.summary.state, JobState::Running);
    assert_eq!(run.summary.runner.as_deref(), Some("r1"));
    assert_eq!(run.summary.platform.as_deref(), Some("linux"));
    assert_eq!(run.summary.started_at, Some(lr_core::Timestamp(START)));
    assert_eq!(run.summary.project, None);
    assert_eq!(run.summary.label, None);
    assert_eq!(run.summary.profile, None);
    assert_eq!(run.summary.source, None);
    assert_eq!(
        (&run.summary.note, &run.summary.branch, &run.summary.cwd),
        (&None, &None, &None)
    );
    assert!(run.steps.is_empty() && run.last_lines.is_empty());
    assert_eq!(s.queue[0].wait_reason, None);
    let hist = history(
        bob.read(ReadRequest::History(HistoryFilter::default()))
            .await,
    );
    assert_eq!(hist.len(), 2);
    assert!(hist
        .iter()
        .all(|j| j.project.is_none() && j.label.is_none()));
    let ReadReply::Job(d) = bob.read(show("j1")).await else {
        panic!("summary shows the run")
    };
    assert_eq!(
        (d.note.as_ref(), d.cwd.as_ref(), d.branch.as_ref()),
        (None, None, None)
    );
    assert_eq!(d.os_user, None);
    assert!(d.steps.is_empty());
    assert!(matches!(
        log_of(&mut bob, "j1").await,
        Err(ReadReply::Denied { .. })
    ));

    let h = Harness::start("[visibility]\nothers_orders = \"full\"\n");
    let (_r, mut alex) = pool_with_jobs(&h).await;
    let mut bob = h.local_hub("bob").await;
    let s = status(bob.read(ReadRequest::Status(StatusFilter::default())).await);
    assert_eq!(s.running[0].summary.project.as_deref(), Some("owner/repo"));
    assert_eq!(s.running[0].summary.label.as_deref(), Some("agent-1"));
    assert_eq!(s.running[0].summary.source, Some(WireSource::Session));
    assert!(history(
        bob.read(ReadRequest::History(HistoryFilter::default()))
            .await
    )
    .iter()
    .all(|j| j.source == Some(WireSource::Session)));
    assert!(s.running[0].last_lines.is_empty());
    let ReadReply::Job(d) = bob.read(show("j1")).await else {
        panic!("full shows the run")
    };
    assert_eq!(d.note.as_deref(), Some("why"));
    assert_eq!(d.cwd.as_deref(), Some("/work/repo"));
    assert!(matches!(
        log_of(&mut bob, "j1").await,
        Err(ReadReply::Denied { .. })
    ));
    assert_eq!(log_of(&mut alex, "j1").await.unwrap(), "secret output\n");
}

#[tokio::test]
async fn broker_owner_sees_everything_including_logs() {
    let h = Harness::start("[visibility]\nothers_orders = \"none\"\n");
    let (_r, _alex) = pool_with_jobs(&h).await;
    let mut root = h.local_hub("root").await;
    let s = status(
        root.read(ReadRequest::Status(StatusFilter::default()))
            .await,
    );
    assert_eq!(s.running[0].summary.project.as_deref(), Some("owner/repo"));
    assert_eq!(s.running[0].last_lines, vec!["secret output".to_string()]);
    assert_eq!(s.queue.len(), 1);
    assert_eq!(log_of(&mut root, "j1").await.unwrap(), "secret output\n");
}

#[tokio::test]
async fn own_runs_are_shown_in_full() {
    let h = Harness::start("[visibility]\nothers_orders = \"none\"\n");
    let (_r, mut alex) = pool_with_jobs(&h).await;
    let s = status(
        alex.read(ReadRequest::Status(StatusFilter::default()))
            .await,
    );
    assert_eq!(s.running[0].summary.profile.as_deref(), Some("unit"));
    assert_eq!(s.running[0].summary.note.as_deref(), Some("why"));
    assert_eq!(s.running[0].summary.cwd.as_deref(), Some("/work/repo"));
    assert_eq!(s.queue[0].summary.cwd.as_deref(), Some("/work/repo"));
    let hist = history(
        alex.read(ReadRequest::History(HistoryFilter::default()))
            .await,
    );
    assert!(hist.iter().all(|j| j.note.as_deref() == Some("why")));
    assert_eq!(s.running[0].steps.len(), 1);
    assert_eq!(s.queue[0].position, 1);
    assert_eq!(s.queue[0].wait_reason.as_deref(), Some("раннер занят"));
    let ReadReply::Job(d) = alex.read(show("j1")).await else {
        panic!("own run")
    };
    assert_eq!(d.os_user.as_deref(), Some("alex"));
    assert_eq!(d.steps[0].name, "build");
}

#[tokio::test]
async fn filter_on_a_hidden_field_never_matches_others_runs() {
    let h = Harness::start("");
    let (_r, mut alex) = pool_with_jobs(&h).await;
    let mut bob = h.local_hub("bob").await;
    let by_project = HistoryFilter {
        project: Some("owner/repo".into()),
        ..HistoryFilter::default()
    };
    assert!(history(bob.read(ReadRequest::History(by_project.clone())).await).is_empty());
    assert_eq!(
        history(alex.read(ReadRequest::History(by_project)).await).len(),
        2
    );
}

#[tokio::test]
async fn status_shows_pool_load_and_queue_positions() {
    let h = Harness::start("");
    let (_r, mut alex) = pool_with_jobs(&h).await;
    alex.send(&order_at("j3", req(512, 1, 60), START + 5)).await;
    alex.accepted().await;
    let s = status(
        alex.read(ReadRequest::Status(StatusFilter::default()))
            .await,
    );
    assert_eq!(
        (
            s.pool.used_cpu,
            s.pool.total_cpu,
            s.pool.used_mem_mb,
            s.pool.total_mem_mb
        ),
        (2, 8, 1024, 16384)
    );
    let ids: Vec<(String, u32)> = s
        .queue
        .iter()
        .map(|q| (q.summary.job.0.clone(), q.position))
        .collect();
    assert_eq!(ids, vec![("j2".into(), 1), ("j3".into(), 2)]);
    let s = status(
        alex.read(ReadRequest::Status(StatusFilter {
            limit: Some(1),
            ..StatusFilter::default()
        }))
        .await,
    );
    assert_eq!(s.queue.len(), 1);
    let next = s.next.expect("more queue");
    let s = status(
        alex.read(ReadRequest::Status(StatusFilter {
            limit: Some(1),
            cursor: Some(next),
            ..StatusFilter::default()
        }))
        .await,
    );
    assert_eq!(s.queue[0].summary.job, JobId("j3".into()));
    assert_eq!(s.next, None);
}

#[tokio::test]
async fn finished_runs_appear_in_recent_and_history() {
    let h = Harness::start("");
    let (mut r, mut alex) = pool_with_jobs(&h).await;
    r.send(&result("j1", 3, JobState::Passed, 0)).await;
    r.assign().await;
    alex.sync().await;
    let s = status(
        alex.read(ReadRequest::Status(StatusFilter::default()))
            .await,
    );
    assert_eq!(s.recent[0].job, JobId("j1".into()));
    assert_eq!(s.recent[0].state, JobState::Passed);
    let hist = history(
        alex.read(ReadRequest::History(HistoryFilter {
            state: Some(JobState::Passed),
            ..HistoryFilter::default()
        }))
        .await,
    );
    assert_eq!(hist.len(), 1);
}

#[tokio::test]
async fn explain_names_the_rule_that_holds_the_order() {
    let h = Harness::start(
        "[[rule]]\nname = \"ci-reserve\"\nwhen = { client_label = \"ci\" }\n\
         reserve = { cpu = 6, mem_mb = 1024 }\n",
    );
    let mut r1 = h.member_runner("r1", "10.0.0.7", linux()).await;
    let _r2 = h
        .member_runner("r2", "10.0.0.8", caps(&["linux"], 8000, 8))
        .await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&order("j1", req(512, 1, 60))).await;
    alex.accepted().await;
    r1.assign().await;
    alex.send(&order("j2", req(512, 2, 60))).await;
    let waiting = alex.accepted().await.waiting.unwrap();
    assert!(waiting.contains("ci-reserve"), "{waiting}");
    let ReadReply::Explain { job, text } = alex
        .read(ReadRequest::Explain {
            job: JobId("j2".into()),
            client: None,
        })
        .await
    else {
        panic!("explain")
    };
    assert_eq!(job, JobId("j2".into()));
    assert!(text.contains("ci-reserve"), "{text}");
}

#[tokio::test]
async fn stats_of_a_client_cover_only_its_own_runs() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    let mut bob = h.local_hub("bob").await;
    for (hub, job) in [(&mut alex, "a1"), (&mut bob, "b1")] {
        hub.send(&order(job, req(512, 1, 60))).await;
        hub.accepted().await;
        r.assign().await;
        r.send(&started(job, 1)).await;
        r.send(&result(job, 2, JobState::Passed, 0)).await;
        r.acked(2).await;
        hub.sync().await;
    }
    let ReadReply::Stats(mine) = bob.read(ReadRequest::Stats(StatsFilter::default())).await else {
        panic!("stats")
    };
    let clients: Vec<&str> = mine.clients.iter().map(|c| c.client.as_str()).collect();
    assert_eq!(clients, vec!["local:bob"]);
    let mut root = h.local_hub("root").await;
    let ReadReply::Stats(all) = root.read(ReadRequest::Stats(StatsFilter::default())).await else {
        panic!("stats")
    };
    assert_eq!(all.clients.len(), 2);
    assert_eq!(all.profiles[0].runs, 2);
}

#[tokio::test]
async fn runners_lists_registered_runners_with_load() {
    let h = Harness::start("");
    let mut gpu = caps(&["linux"], 16384, 8);
    gpu.tags.insert("gpu".into());
    let mut r = h.member_runner("r1", "10.0.0.7", gpu).await;
    let off = h.member_runner("r2", "10.0.0.8", linux()).await;
    off.close().await;
    let mut alex = h.local_hub("alex").await;
    alex.until_runner("r2", false).await;
    let mut want = req(1024, 2, 60);
    want.tags.insert("gpu".into());
    alex.send(&order("j1", want)).await;
    alex.accepted().await;
    r.assign().await;
    let ReadReply::Runners { runners } = alex.read(ReadRequest::Runners).await else {
        panic!("runners")
    };
    let r1 = runners.iter().find(|x| x.name == "r1").unwrap();
    assert!(r1.online);
    assert!(r1.caps.tags.contains("gpu"));
    assert_eq!((r1.free_mem_mb, r1.free_cpu), (16384 - 1024, 6));
    assert_eq!(r1.held_tags[0].tag, "gpu");
    assert_eq!(r1.held_tags[0].job, JobId("j1".into()));
    assert!(!runners.iter().find(|x| x.name == "r2").unwrap().online);
}

#[tokio::test]
async fn capabilities_query_reports_runner_zones_relative_to_the_hub() {
    let h = Harness::start("[networks]\nhome = [\"10.0.0.0/24\"]\n");
    let _lan = h.member_runner("r-lan", "10.0.0.7", linux()).await;
    let _far = h.member_runner("r-far", "172.16.0.9", linux()).await;
    let mut hub = h.member_hub("hub1", "", "10.0.0.5").await;
    hub.send(&HubToBroker::CapabilitiesQuery(CapabilitiesQuery {
        req: 4,
    }))
    .await;
    let reply = loop {
        if let BrokerToHub::CapabilitiesReply(c) = hub.hub().await {
            break c;
        }
    };
    assert_eq!(reply.req, 4);
    assert_eq!(reply.zone, Zone::Lan);
    let zone = |n: &str| reply.runners.iter().find(|r| r.name == n).unwrap().zone;
    assert_eq!(zone("r-lan"), Zone::Lan);
    assert_eq!(zone("r-far"), Zone::Remote);
    assert!(reply.runners.iter().all(|r| r.online));
}

#[tokio::test]
async fn subscription_delivers_events_filtered_by_visibility() {
    let h = Harness::start("");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    let mut bob = h.local_hub("bob").await;
    let topics = || ReadRequest::Subscribe {
        topics: vec![Topic::Queue],
    };
    let ReadReply::Subscribed { sub } = bob.read(topics()).await else {
        panic!("subscribed")
    };
    alex.read(topics()).await;
    alex.send(&order("j1", req(512, 1, 60))).await;
    let mine = loop {
        if let BrokerToHub::Event(e) = alex.hub().await {
            break e;
        }
    };
    let QueueEvent::JobQueued { job } = mine.event else {
        panic!("queued first")
    };
    assert_eq!(job.project.as_deref(), Some("owner/repo"));
    let theirs = loop {
        if let BrokerToHub::Event(e) = bob.hub().await {
            break e;
        }
    };
    assert_eq!(theirs.sub, sub);
    let QueueEvent::JobQueued { job } = theirs.event else {
        panic!("queued first")
    };
    assert_eq!(job.project, None);
    assert_eq!(
        bob.read(ReadRequest::Unsubscribe { sub }).await,
        ReadReply::Unsubscribed { sub }
    );
    alex.send(&order("j2", req(512, 1, 60))).await;
    alex.accepted().await;
    assert!(bob
        .sync()
        .await
        .iter()
        .all(|m| !matches!(m, BrokerToHub::Event(_))));
}

#[tokio::test]
async fn subscription_hides_others_events_at_visibility_none() {
    let h = Harness::start("[visibility]\nothers_orders = \"none\"\n");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    let mut bob = h.local_hub("bob").await;
    bob.read(ReadRequest::Subscribe {
        topics: vec![Topic::Queue, Topic::Runners],
    })
    .await;
    alex.send(&order("j1", req(512, 1, 60))).await;
    alex.accepted().await;
    let got = bob.sync().await;
    assert!(
        got.iter().all(|m| !matches!(m, BrokerToHub::Event(_))),
        "{got:?}"
    );
}

#[tokio::test]
async fn bus_publishes_the_job_lifecycle() {
    let mut h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&order("j1", req(512, 1, 60))).await;
    alex.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    r.send(&result("j1", 2, JobState::Passed, 0)).await;
    alex.sync().await;
    let mut kinds = Vec::new();
    while let Ok(BusEvent { client, event }) = h.events.try_recv() {
        match event {
            QueueEvent::JobQueued { .. } => kinds.push("queued"),
            QueueEvent::JobStarted { .. } => kinds.push("started"),
            QueueEvent::JobFinished { state, .. } => {
                assert_eq!(state, JobState::Passed);
                kinds.push("finished")
            }
            _ => continue,
        }
        assert_eq!(client.as_deref(), Some("local:alex"));
    }
    assert_eq!(kinds, vec!["queued", "started", "finished"]);
}

#[tokio::test]
async fn unknown_read_request_gets_unknown_reply() {
    let h = Harness::start("");
    let mut alex = h.local_hub("alex").await;
    alex.send_raw("{\"type\":\"read\",\"req\":1,\"request\":{\"type\":\"future_thing\"}}")
        .await;
    let BrokerToHub::ReadReply(a) = alex.hub().await else {
        panic!("reply")
    };
    assert_eq!(a.reply, ReadReply::Unknown);
}
