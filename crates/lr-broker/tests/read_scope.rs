mod common;

use common::harness::*;
use lr_core::{JobId, JobState, MachineRef};
use lr_proto::*;

async fn running(h: &Harness) -> (Conn, Conn) {
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    hub.started_notice().await;
    (r, hub)
}

async fn log_reply(c: &mut Conn, step: Option<&str>, follow: bool) -> Result<String, ReadReply> {
    c.send(&HubToBroker::Read(ReadCall {
        req: 77,
        request: ReadRequest::Log {
            job: JobId("j1".into()),
            step: step.map(str::to_string),
            tail: None,
            follow,
            client: None,
        },
    }))
    .await;
    let mut text = String::new();
    loop {
        match c.hub().await {
            BrokerToHub::LogChunk(l) if l.req == 77 => {
                text.push_str(&l.text);
                if l.end {
                    return Ok(text);
                }
            }
            BrokerToHub::ReadReply(a) if a.req == 77 => return Err(a.reply),
            _ => {}
        }
    }
}

#[tokio::test]
async fn log_step_filter_returns_only_that_step() {
    let h = Harness::start("");
    let (mut r, mut hub) = running(&h).await;
    r.send(&chunk("j1", 2, "=== step build | project=o/r ===\nb1\n"))
        .await;
    r.send(&chunk("j1", 3, "b2\n=== step test ===\nt1\n")).await;
    r.send(&chunk("j1", 4, "=== step test cancelled at 5 ===\n"))
        .await;
    r.acked(4).await;
    hub.sync().await;
    assert_eq!(
        log_reply(&mut hub, Some("build"), false).await.unwrap(),
        "=== step build | project=o/r ===\nb1\nb2\n"
    );
    assert_eq!(
        log_reply(&mut hub, Some("test"), false).await.unwrap(),
        "=== step test ===\nt1\n=== step test cancelled at 5 ===\n"
    );
    assert_eq!(log_reply(&mut hub, Some("nope"), false).await.unwrap(), "");
}

#[tokio::test]
async fn status_reads_only_the_tail_of_a_running_log() {
    let h = Harness::start("");
    let (mut r, mut hub) = running(&h).await;
    r.send(&chunk("j1", 2, "one\ntwo\n")).await;
    r.acked(2).await;
    let ReadReply::Status(s) = hub.read(ReadRequest::Status(StatusFilter::default())).await else {
        panic!("status");
    };
    assert_eq!(s.running[0].last_lines, vec!["one", "two"]);
    assert_eq!(h.faults.reads(), 0);
}

#[tokio::test]
async fn history_and_lookups_scan_a_bounded_number_of_rows() {
    let h = Harness::start("");
    let (_r, mut hub) = running(&h).await;
    hub.read(ReadRequest::History(HistoryFilter::default()))
        .await;
    let mut bob = h.local_hub("bob").await;
    bob.read(ReadRequest::HistoryShow {
        job: JobId("j1".into()),
        client: None,
    })
    .await;
    let filters = h.faults.seen_filters();
    assert!(!filters.is_empty());
    assert!(filters.iter().all(|f| f.limit.is_some()), "{filters:?}");
}

#[tokio::test]
async fn local_runner_machine_is_hidden_from_other_clients() {
    let h = Harness::start("");
    let _mine = h.local_runner("alex", "lr", linux()).await;
    let machine_seen = |runners: &[RunnerInfo]| runners[0].caps.machine.clone();
    for (user, expected) in [
        ("bob", None),
        ("alex", Some(MachineRef("local:alex".into()))),
        ("root", Some(MachineRef("local:alex".into()))),
    ] {
        let mut c = h.local_hub(user).await;
        let ReadReply::Runners { runners } = c.read(ReadRequest::Runners).await else {
            panic!("runners");
        };
        assert_eq!(machine_seen(&runners), expected, "{user}");
        c.send(&HubToBroker::CapabilitiesQuery(CapabilitiesQuery {
            req: 5,
        }))
        .await;
        let caps = loop {
            if let BrokerToHub::CapabilitiesReply(r) = c.hub().await {
                break r;
            }
        };
        assert_eq!(caps.runners[0].caps.machine, expected, "{user}");
    }
}

#[tokio::test]
async fn show_needs_the_client_when_two_clients_use_one_job_id() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    let mut bob = h.local_hub("bob").await;
    for (c, at) in [(&mut alex, START), (&mut bob, START + 5)] {
        c.send(&order_at("j1", req(512, 1, 60), at)).await;
        c.accepted().await;
        let a = r.assign().await;
        r.send(&started("j1", 1)).await;
        r.send(&result("j1", 2, JobState::Passed, 0)).await;
        r.acked(2).await;
        assert_eq!(a.job, JobId("j1".into()));
    }
    let mut root = h.local_hub("root").await;
    let show = |client: Option<&str>| ReadRequest::HistoryShow {
        job: JobId("j1".into()),
        client: client.map(str::to_string),
    };
    assert!(
        matches!(root.read(show(None)).await, ReadReply::NotFound { what } if what.contains("local:bob"))
    );
    match root.read(show(Some("local:bob"))).await {
        ReadReply::Job(d) => assert_eq!(d.summary.queued_at, lr_core::Timestamp(START + 5)),
        other => panic!("expected the run, got {other:?}"),
    }
}

fn history_ids(reply: ReadReply) -> Vec<String> {
    match reply {
        ReadReply::History { jobs, .. } => jobs.into_iter().map(|j| j.job.0).collect(),
        other => panic!("expected history, got {other:?}"),
    }
}

#[tokio::test]
async fn history_mine_shows_only_the_askers_runs_whatever_the_visibility() {
    let h = Harness::start("[visibility]\nothers_orders = \"full\"\n");
    let (_r, _alex) = running(&h).await;
    let mut bob = h.local_hub("bob").await;
    bob.send(&order("b1", req(512, 1, 60))).await;
    bob.accepted().await;
    let all = history_ids(
        bob.read(ReadRequest::History(HistoryFilter::default()))
            .await,
    );
    assert!(all.contains(&"j1".to_string()) && all.contains(&"b1".to_string()));
    let mine = HistoryFilter {
        mine: true,
        ..HistoryFilter::default()
    };
    assert_eq!(
        history_ids(bob.read(ReadRequest::History(mine)).await),
        ["b1"]
    );
    let mine_over_client = HistoryFilter {
        mine: true,
        client: Some("local:alex".into()),
        ..HistoryFilter::default()
    };
    assert_eq!(
        history_ids(bob.read(ReadRequest::History(mine_over_client)).await),
        ["b1"],
        "mine wins over a named client"
    );
}
