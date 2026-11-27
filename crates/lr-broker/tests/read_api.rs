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
        ReadReply::Status(s) => *s,
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
    assert_eq!(
        s.queue[0].wait_reason.as_deref(),
        Some("the runner is busy")
    );
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
    let ReadReply::Explain { job, text, .. } = alex
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
async fn labels_show_to_the_owner_and_are_masked_for_others() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&order_labelled(
        "j1",
        req(1024, 2, 60),
        &["release", "work"],
    ))
    .await;
    alex.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    alex.send(&order_labelled("j2", req(512, 1, 60), &["work"]))
        .await;
    alex.accepted().await;
    alex.sync().await;
    let mine = status(
        alex.read(ReadRequest::Status(StatusFilter::default()))
            .await,
    );
    assert_eq!(mine.running[0].summary.labels, vec!["release", "work"]);
    assert_eq!(mine.queue[0].summary.labels, vec!["work"]);
    let ReadReply::Explain { text, .. } = alex
        .read(ReadRequest::Explain {
            job: JobId("j2".into()),
            client: None,
        })
        .await
    else {
        panic!("explain")
    };
    assert!(text.contains("work"), "{text}");
    let mut bob = h.local_hub("bob").await;
    let theirs = status(bob.read(ReadRequest::Status(StatusFilter::default())).await);
    assert!(theirs.running[0].summary.labels.is_empty());
    assert!(theirs.queue[0].summary.labels.is_empty());
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
    let ReadReply::Runners { runners, .. } = alex.read(ReadRequest::Runners).await else {
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
        probe: None,
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

fn explain_text(reply: ReadReply) -> String {
    match reply {
        ReadReply::Explain { text, .. } => text,
        other => panic!("expected explain, got {other:?}"),
    }
}

fn explain_of(job: &str) -> ReadRequest {
    ReadRequest::Explain {
        job: JobId(job.into()),
        client: None,
    }
}

async fn pass_time_heard(h: &Harness, r: &mut Conn, alex: &mut Conn, steps: u64) {
    for _ in 0..steps {
        h.clock.advance(std::time::Duration::from_secs(20));
        r.send(&RunnerToBroker::Heartbeat { load: None }).await;
        alex.sync().await;
    }
}

#[tokio::test]
async fn explain_of_a_granted_order_says_how_long_it_waits_and_when_the_runner_answered() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&order("j1", req(512, 1, 60))).await;
    alex.accepted().await;
    r.assign().await;
    pass_time_heard(&h, &mut r, &mut alex, 5).await;
    h.clock.advance(std::time::Duration::from_secs(20));
    let text = explain_text(alex.read(explain_of("j1")).await);
    assert!(text.contains("assigned to runner r1"), "{text}");
    assert!(text.contains("waiting to start for 2 min"), "{text}");
    assert!(text.contains("the runner answered 20 s ago"), "{text}");
    assert!(
        !text.contains("stale") && !text.contains("offline"),
        "{text}"
    );
}

#[tokio::test]
async fn explain_names_the_job_that_holds_the_only_slot() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&order("j1", req(512, 1, 60))).await;
    alex.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    alex.send(&order("j2", req(512, 1, 60))).await;
    assert_eq!(
        alex.accepted().await.waiting.as_deref(),
        Some("the runner is busy")
    );
    pass_time_heard(&h, &mut r, &mut alex, 9).await;
    let text = explain_text(alex.read(explain_of("j2")).await);
    assert!(
        text.contains("the runner is busy with order j1 (profile "),
        "{text}"
    );
    assert!(text.contains("3 min of 1 min"), "{text}");
    assert!(text.contains("; no later than "), "{text}");
}

#[tokio::test]
async fn explain_gives_the_structured_detail_and_the_upper_bound_of_the_wait() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&order("j1", req(512, 1, 600))).await;
    alex.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    alex.send(&order("j2", req(512, 1, 300))).await;
    alex.accepted().await;
    alex.send(&order("j3", req(512, 1, 120))).await;
    alex.accepted().await;
    pass_time_heard(&h, &mut r, &mut alex, 9).await;
    let ReadReply::Explain { detail, .. } = alex.read(explain_of("j3")).await else {
        panic!("explain")
    };
    let detail = detail.expect("a job that waits for the runner has a detail");
    let running: Vec<_> = detail.blocking.iter().filter(|b| b.running).collect();
    assert_eq!(running.len(), 1, "{detail:?}");
    assert_eq!(running[0].job.0, "j1");
    assert_eq!(running[0].timeout_s, 600);
    assert!(running[0].elapsed_s >= 180, "{detail:?}");
    assert_eq!(
        detail.bound_s,
        (600 - running[0].elapsed_s) + 300,
        "the rest of j1 plus the whole timeout of j2 ahead, not the own timeout of j3"
    );
    assert_eq!(detail.unnamed, 0);
    let mut line = None;
    let ReadReply::Status(status) = alex
        .read(ReadRequest::Status(StatusFilter::default()))
        .await
    else {
        panic!("status")
    };
    for q in &status.queue {
        if q.summary.job.0 == "j3" {
            line = q.detail.clone();
            assert!(q.requested.is_some());
        }
    }
    assert_eq!(line, Some(detail));
    assert!(status.running[0].requested.is_some());
}

#[tokio::test]
async fn the_bound_sums_foreign_jobs_a_client_may_not_see_without_naming_them() {
    let h = Harness::start("[visibility]\nothers_orders = \"none\"\n");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    let mut ci = h.member_hub("ci-box", "ci", "10.0.0.20").await;
    alex.send(&order("j1", req(512, 1, 600))).await;
    alex.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    ci.send(&order("c1", req(512, 1, 120))).await;
    ci.accepted().await;
    let ReadReply::Explain { detail, text, .. } = ci.read(explain_of("c1")).await else {
        panic!("explain")
    };
    let detail = detail.expect("detail");
    assert!(detail.blocking.is_empty(), "{detail:?}");
    assert_eq!(detail.unnamed, 1);
    assert!(detail.bound_s > 0 && detail.bound_s <= 600, "{detail:?}");
    assert!(!text.contains("j1"), "{text}");
}

#[tokio::test]
async fn explain_says_nothing_of_a_foreign_holder_a_client_may_not_see() {
    let h = Harness::start("[visibility]\nothers_orders = \"none\"\n");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    let mut ci = h.member_hub("ci-box", "ci", "10.0.0.20").await;
    alex.send(&order("j1", req(512, 1, 60))).await;
    alex.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    ci.send(&order("c1", req(512, 1, 60))).await;
    ci.accepted().await;
    alex.send(&order("j2", req(512, 1, 60))).await;
    alex.accepted().await;
    let seen_by_ci = explain_text(ci.read(explain_of("c1")).await);
    let seen_by_ci = seen_by_ci
        .split("; priority:")
        .next()
        .unwrap_or_default()
        .split("; no later than")
        .next()
        .unwrap_or_default()
        .to_string();
    assert!(seen_by_ci.contains("the runner is busy"), "{seen_by_ci}");
    assert!(!seen_by_ci.contains("j1"), "{seen_by_ci}");
    assert!(!seen_by_ci.contains("order"), "{seen_by_ci}");
    assert!(
        !seen_by_ci.contains('(') && !seen_by_ci.contains("min"),
        "the age of the hidden job leaks: {seen_by_ci}"
    );
    let seen_by_owner = explain_text(alex.read(explain_of("j2")).await);
    assert!(
        seen_by_owner.contains("the runner is busy with order j1 ("),
        "{seen_by_owner}"
    );
}

const ALEX: &str = "local:alex";
const TEN_S: std::time::Duration = std::time::Duration::from_secs(10);
const MIN: u64 = 60_000;

fn bash_needed() -> lr_core::Requirements {
    let mut want = req(512, 1, 60);
    want.programs.push(lr_core::Program {
        name: "bash".into(),
        check: None,
    });
    want
}

fn sender_gone(job: &str, seq: u64) -> HubToBroker {
    let HubToBroker::Cancel(mut c) = cancel(job, seq) else {
        unreachable!()
    };
    c.code = ReasonCode::SenderGone;
    c.reason = None;
    HubToBroker::Cancel(c)
}

async fn cancel_acked(c: &mut Conn) {
    loop {
        if let BrokerToHub::CancelAck(_) = c.hub().await {
            return;
        }
    }
}

fn recent_of(s: &StatusView) -> Vec<(&str, JobState, Option<ReasonCode>, Option<&str>)> {
    s.recent
        .iter()
        .map(|j| {
            (
                j.job.0.as_str(),
                j.state,
                j.reason.clone(),
                j.cancelled_by.as_deref(),
            )
        })
        .collect()
}

#[tokio::test]
async fn recent_lists_cancelled_withdrawn_and_rejected_orders_with_actor_and_reason() {
    let h = Harness::start("");
    let (r, mut alex) = pool_with_jobs(&h).await;
    alex.send(&order("j3", req(512, 1, 60))).await;
    alex.accepted().await;
    h.clock.advance(TEN_S);
    alex.send(&sender_gone("j3", 2)).await;
    cancel_acked(&mut alex).await;
    h.clock.advance(TEN_S);
    alex.send(&cancel("j2", 2)).await;
    cancel_acked(&mut alex).await;
    h.clock.advance(TEN_S);
    alex.send(&order("j4", bash_needed())).await;
    let refused = alex.rejected().await;
    assert!(refused.detail.contains("bash"), "{}", refused.detail);
    h.clock.advance(TEN_S);
    r.send(&result("j1", 3, JobState::Passed, 0)).await;
    h.until_state(ALEX, "j1", JobState::Passed).await;
    let s = status(
        alex.read(ReadRequest::Status(StatusFilter::default()))
            .await,
    );
    assert_eq!(
        recent_of(&s),
        vec![
            ("j1", JobState::Passed, None, None),
            (
                "j4",
                JobState::Cancelled,
                Some(ReasonCode::Rejected(RejectReason::NoMatchingRunner)),
                Some("broker")
            ),
            (
                "j2",
                JobState::Cancelled,
                Some(ReasonCode::CancelledByUser),
                Some("agent-1")
            ),
            (
                "j3",
                JobState::Cancelled,
                Some(ReasonCode::SenderGone),
                Some("agent-1")
            ),
        ],
        "newest end first, each with its reason and who ended it"
    );
    let mut bob = h.local_hub("bob").await;
    let theirs = status(bob.read(ReadRequest::Status(StatusFilter::default())).await);
    assert_eq!(theirs.recent.len(), 4);
    assert!(
        theirs.recent.iter().all(|j| j.cancelled_by.is_none()),
        "the canceller is hidden at summary visibility"
    );
}

#[tokio::test]
async fn history_offset_skips_runs_and_the_answer_counts_them_all() {
    let h = Harness::start("");
    let (r, mut alex) = pool_with_jobs(&h).await;
    alex.send(&order("j3", req(512, 1, 60))).await;
    alex.accepted().await;
    h.clock.advance(TEN_S);
    alex.send(&cancel("j2", 2)).await;
    cancel_acked(&mut alex).await;
    h.clock.advance(TEN_S);
    r.send(&result("j1", 3, JobState::Passed, 0)).await;
    h.until_state(ALEX, "j1", JobState::Passed).await;
    let page = |limit: u64, offset: Option<u64>| {
        ReadRequest::History(HistoryFilter {
            limit: Some(limit),
            offset,
            ..HistoryFilter::default()
        })
    };
    let ReadReply::History { jobs, next, total } = alex.read(page(1, Some(1))).await else {
        panic!("history")
    };
    let ids: Vec<&str> = jobs.iter().map(|j| j.job.0.as_str()).collect();
    assert_eq!(ids, ["j2"], "the second most recently finished");
    assert_eq!((next.as_deref(), total), (Some("2"), Some(3)));
    let ReadReply::History { jobs, next, total } = alex.read(page(2, Some(2))).await else {
        panic!("history")
    };
    let ids: Vec<&str> = jobs.iter().map(|j| j.job.0.as_str()).collect();
    assert_eq!(ids, ["j3"]);
    assert_eq!((next, total), (None, Some(3)));
    let ReadReply::History { jobs, total, .. } = alex.read(page(5, Some(50))).await else {
        panic!("history")
    };
    assert!(jobs.is_empty(), "past the end");
    assert_eq!(total, Some(3));
    let ReadReply::Details { jobs, total, .. } = alex
        .read(ReadRequest::HistoryDetails(HistoryFilter {
            limit: Some(1),
            offset: Some(2),
            ..HistoryFilter::default()
        }))
        .await
    else {
        panic!("details")
    };
    assert_eq!(jobs.len(), 1);
    assert_eq!(total, Some(3));
}

#[tokio::test]
async fn status_counts_the_whole_queue_whatever_the_size_of_the_list() {
    let h = Harness::start("");
    let (_r, mut alex) = pool_with_jobs(&h).await;
    alex.send(&order("j3", req(512, 1, 60))).await;
    alex.accepted().await;
    let s = status(
        alex.read(ReadRequest::Status(StatusFilter {
            limit: Some(1),
            ..StatusFilter::default()
        }))
        .await,
    );
    assert_eq!(s.queue.len(), 1);
    assert!(s.next.is_some(), "the list was cut");
    assert_eq!(s.queue_total, Some(2), "j2 and j3 wait");
}

#[tokio::test]
async fn history_window_keeps_the_runs_that_were_running_in_it() {
    let h = Harness::start("");
    let (r, mut alex) = pool_with_jobs(&h).await;
    let ids =
        |jobs: Vec<JobSummary>| -> Vec<String> { jobs.into_iter().map(|j| j.job.0).collect() };
    let at = |ms: u64| Some(lr_core::Timestamp(START + ms));
    let window = |since: Option<u64>, until: Option<u64>| {
        ReadRequest::History(HistoryFilter {
            ran_since: since.and_then(at),
            ran_until: until.and_then(at),
            ..HistoryFilter::default()
        })
    };
    assert_eq!(
        ids(history(alex.read(window(Some(5_000), None)).await)),
        ["j1"],
        "a run still going overlaps; an order that never started does not"
    );
    h.clock.advance(TEN_S);
    r.send(&result("j1", 3, JobState::Passed, 0)).await;
    h.until_state(ALEX, "j1", JobState::Passed).await;
    assert_eq!(
        ids(history(alex.read(window(Some(5_000), Some(8_000))).await)),
        ["j1"]
    );
    assert!(history(alex.read(window(Some(60_000), None)).await).is_empty());
    let before = ReadRequest::History(HistoryFilter {
        ran_until: Some(lr_core::Timestamp(START - 1)),
        ..HistoryFilter::default()
    });
    assert!(
        history(alex.read(before).await).is_empty(),
        "nothing had started before the window ended"
    );
}

#[tokio::test]
async fn history_lists_the_most_recently_finished_first_and_a_limit_keeps_them() {
    let h = Harness::start("");
    let (r, mut alex) = pool_with_jobs(&h).await;
    alex.send(&order("j3", req(512, 1, 60))).await;
    alex.accepted().await;
    h.clock.advance(TEN_S);
    alex.send(&cancel("j2", 2)).await;
    cancel_acked(&mut alex).await;
    h.clock.advance(TEN_S);
    r.send(&result("j1", 3, JobState::Passed, 0)).await;
    h.until_state(ALEX, "j1", JobState::Passed).await;
    let ids =
        |jobs: Vec<JobSummary>| -> Vec<String> { jobs.into_iter().map(|j| j.job.0).collect() };
    let ask = |limit: Option<u64>| {
        ReadRequest::History(HistoryFilter {
            limit,
            ..HistoryFilter::default()
        })
    };
    assert_eq!(
        ids(history(alex.read(ask(None)).await)),
        ["j1", "j2", "j3"],
        "the run that finished last comes first, an unfinished one last"
    );
    assert_eq!(ids(history(alex.read(ask(Some(1))).await)), ["j1"]);
    assert_eq!(ids(history(alex.read(ask(Some(2))).await)), ["j1", "j2"]);
    let details = alex
        .read(ReadRequest::HistoryDetails(HistoryFilter {
            limit: Some(2),
            ..HistoryFilter::default()
        }))
        .await;
    match details {
        ReadReply::Details { jobs, .. } => {
            let ids: Vec<&str> = jobs.iter().map(|d| d.summary.job.0.as_str()).collect();
            assert_eq!(ids, ["j1", "j2"]);
        }
        other => panic!("expected details, got {other:?}"),
    }
}

#[tokio::test]
async fn recent_honours_a_count_and_a_duration() {
    let h = Harness::start("");
    for (i, job) in ["a1", "a2", "a3"].iter().enumerate() {
        if i > 0 {
            h.clock.advance(std::time::Duration::from_millis(10 * MIN));
        }
        let mut alex = h.local_hub("alex").await;
        alex.send(&order(job, req(512, 1, 60))).await;
        alex.rejected().await;
    }
    h.clock.advance(std::time::Duration::from_millis(5 * MIN));
    let mut alex = h.local_hub("alex").await;
    let ask = |count: Option<u64>, since: Option<u64>| {
        ReadRequest::Status(StatusFilter {
            recent: Some(RecentFilter {
                count,
                since: since.map(|m| lr_core::Timestamp(START + m * MIN)),
            }),
            ..StatusFilter::default()
        })
    };
    let ids = |s: StatusView| -> Vec<String> { s.recent.into_iter().map(|j| j.job.0).collect() };
    assert_eq!(
        ids(status(alex.read(ask(Some(2), None)).await)),
        ["a3", "a2"]
    );
    assert_eq!(ids(status(alex.read(ask(None, Some(15))).await)), ["a3"]);
    assert_eq!(
        ids(status(alex.read(ask(None, Some(5))).await)),
        ["a3", "a2"]
    );
    assert_eq!(ids(status(alex.read(ask(Some(1), Some(0))).await)), ["a3"]);
    assert_eq!(
        ids(status(alex.read(ask(None, None)).await)),
        ["a3", "a2", "a1"]
    );
}

#[tokio::test]
async fn explain_answers_for_a_rejected_order_and_names_the_unmet_requirement() {
    let h = Harness::start("");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&order("j9", bash_needed())).await;
    alex.rejected().await;
    let text = explain_text(alex.read(explain_of("j9")).await);
    assert!(text.contains("rejected at intake"), "{text}");
    assert!(text.contains("rejected_no_matching_runner"), "{text}");
    assert!(text.contains("r1: bash not found"), "{text}");
    assert!(text.contains("by: broker"), "{text}");
    match alex.read(explain_of("never-sent")).await {
        ReadReply::NotFound { what } => assert_eq!(what, "job never-sent"),
        other => panic!("expected not found, got {other:?}"),
    }
}

#[tokio::test]
async fn explain_of_a_cancelled_order_names_who_cancelled_it_and_why() {
    let h = Harness::start("");
    let (_r, mut alex) = pool_with_jobs(&h).await;
    alex.send(&cancel("j2", 2)).await;
    cancel_acked(&mut alex).await;
    let text = explain_text(alex.read(explain_of("j2")).await);
    assert!(text.contains("cancelled"), "{text}");
    assert!(text.contains("cancelled_by_user"), "{text}");
    assert!(text.contains("by: agent-1"), "{text}");
    assert!(text.contains("stop"), "{text}");
}

async fn runners_and_pools(c: &mut Conn) -> (Vec<RunnerInfo>, Vec<PoolInfo>) {
    match c.read(ReadRequest::Runners).await {
        ReadReply::Runners { runners, pools } => (runners, pools),
        other => panic!("runners, got {other:?}"),
    }
}

fn gpu_runner() -> WireCapabilities {
    let mut c = linux();
    c.tags.insert("gpu".into());
    c
}

fn mac_runner() -> WireCapabilities {
    caps(&["mac"], 16384, 8)
}

#[tokio::test]
async fn two_runners_of_one_pool_are_grouped_with_one_shared_budget() {
    let h = Harness::start("");
    let mut r1 = h.member_runner("r1", "10.0.0.7", gpu_runner()).await;
    let _r2 = h.member_runner("r2", "10.0.0.8", mac_runner()).await;
    let mut alex = h.local_hub("alex").await;
    let mut want = req(1024, 2, 60);
    want.tags.insert("gpu".into());
    alex.send(&order("j1", want)).await;
    alex.accepted().await;
    r1.assign().await;
    let (runners, pools) = runners_and_pools(&mut alex).await;
    assert_eq!(pools.len(), 1, "{pools:?}");
    let pool = &pools[0];
    assert_eq!(pool.name, "default");
    assert_eq!((pool.mem_total_mb, pool.mem_free_mb), (16384, 16384 - 1024));
    assert_eq!((pool.cpu_total, pool.cpu_free), (8, 6));
    assert_eq!(pool.runners, ["r1".to_string(), "r2".to_string()]);
    assert_eq!(pool.held_tags.len(), 1);
    assert_eq!(pool.held_tags[0].tag, "gpu");
    assert_eq!(pool.held_tags[0].job, JobId("j1".into()));
    assert!(runners.iter().all(|r| r.pool.as_deref() == Some("default")));
    let (r1, r2) = (
        runners.iter().find(|r| r.name == "r1").unwrap(),
        runners.iter().find(|r| r.name == "r2").unwrap(),
    );
    assert_eq!((r1.busy, r2.busy), (Some(true), Some(false)));
    assert_eq!(
        (r1.can_start_now, r2.can_start_now),
        (Some(false), Some(true))
    );
    assert_eq!(r1.free_mem_mb, pool.mem_free_mb);
}

#[tokio::test]
async fn an_idle_runner_cannot_start_a_job_when_the_pool_is_full() {
    let h = Harness::start("");
    let mut r1 = h.member_runner("r1", "10.0.0.7", linux()).await;
    let _r2 = h.member_runner("r2", "10.0.0.8", mac_runner()).await;
    let mut alex = h.local_hub("alex").await;
    let (before, _) = runners_and_pools(&mut alex).await;
    let idle = before.iter().find(|r| r.name == "r2").unwrap();
    assert_eq!((idle.busy, idle.can_start_now), (Some(false), Some(true)));
    alex.send(&order("j1", req(1024, 8, 60))).await;
    alex.accepted().await;
    r1.assign().await;
    let (after, pools) = runners_and_pools(&mut alex).await;
    let idle = after.iter().find(|r| r.name == "r2").unwrap();
    assert_eq!(idle.busy, Some(false));
    assert_eq!(idle.can_start_now, Some(false));
    assert_eq!(pools[0].cpu_free, 0);
}

#[tokio::test]
async fn foreign_jobs_are_summarised_for_a_viewer_with_summary_visibility() {
    for (others, shown) in [("summary", false), ("none", false), ("full", true)] {
        let h = Harness::start(&format!("[visibility]\nothers_orders = \"{others}\"\n"));
        let mut r1 = h.member_runner("r1", "10.0.0.7", gpu_runner()).await;
        let mut alex = h.local_hub("alex").await;
        let mut bob = h.local_hub("bob").await;
        let mut want = req(1024, 2, 60);
        want.tags.insert("gpu".into());
        alex.send(&order("j1", want)).await;
        alex.accepted().await;
        r1.assign().await;
        let (bobs_runners, seen_by_bob) = runners_and_pools(&mut bob).await;
        let on_runner = &bobs_runners[0].held_tags;
        assert_eq!(on_runner.len(), 1, "{others}");
        assert_eq!(on_runner[0].tag, "gpu", "{others}");
        assert_eq!(!on_runner[0].job.0.is_empty(), shown, "{others}");
        let pool = &seen_by_bob[0];
        assert_eq!(
            (pool.mem_free_mb, pool.cpu_free),
            (16384 - 1024, 6),
            "{others}"
        );
        assert_eq!(pool.held_tags.len(), 1, "{others}");
        assert_eq!(pool.held_tags[0].tag, "gpu", "{others}");
        let id = &pool.held_tags[0].job;
        assert_eq!(!id.0.is_empty(), shown, "{others}: {id:?}");
        let (alexs_runners, seen_by_alex) = runners_and_pools(&mut alex).await;
        assert_eq!(seen_by_alex[0].held_tags[0].job, JobId("j1".into()));
        assert_eq!(
            alexs_runners[0].held_tags[0].job,
            JobId("j1".into()),
            "{others}"
        );
    }
}

#[tokio::test]
async fn an_unknown_canceller_is_absent_not_empty() {
    let h = Harness::start("");
    let (_r, mut alex) = pool_with_jobs(&h).await;
    let HubToBroker::Cancel(mut c) = cancel("j2", 2) else {
        unreachable!()
    };
    c.by.label = String::new();
    alex.send(&HubToBroker::Cancel(c)).await;
    cancel_acked(&mut alex).await;
    let row = h.until_state(ALEX, "j2", JobState::Cancelled).await;
    assert_eq!(row.job.cancelled_by, None, "an empty actor was stored");
    let runs = history(
        alex.read(ReadRequest::History(HistoryFilter::default()))
            .await,
    );
    let j2 = runs.iter().find(|s| s.job.0 == "j2").unwrap();
    assert_eq!(j2.cancelled_by, None);
    let text = explain_text(alex.read(explain_of("j2")).await);
    assert!(!text.contains("by:"), "{text}");
}

#[tokio::test]
async fn explain_of_a_running_job_names_the_zone_of_its_runner() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&order("j1", req(512, 1, 60))).await;
    alex.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    alex.sync().await;
    let ReadReply::Explain { text, .. } = alex
        .read(ReadRequest::Explain {
            job: JobId("j1".into()),
            client: None,
        })
        .await
    else {
        panic!("explain")
    };
    assert!(
        text.starts_with("running on runner r1 (zone ") && text.ends_with(')'),
        "{text}"
    );
}
