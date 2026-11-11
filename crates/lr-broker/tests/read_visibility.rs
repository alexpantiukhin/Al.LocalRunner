mod common;

use common::harness::*;
use lr_core::{JobId, JobState};
use lr_proto::*;

fn show(job: &str) -> ReadRequest {
    ReadRequest::HistoryShow {
        job: JobId(job.into()),
        client: None,
    }
}

fn history(reply: ReadReply) -> Vec<JobSummary> {
    match reply {
        ReadReply::History { jobs, .. } => jobs,
        other => panic!("expected history, got {other:?}"),
    }
}

async fn log_reply(c: &mut Conn, job: &str) -> ReadReply {
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
    loop {
        match c.hub().await {
            BrokerToHub::LogChunk(l) if l.req == 900 => {
                return ReadReply::Failed {
                    message: format!("log streamed: {}", l.text),
                }
            }
            BrokerToHub::ReadReply(a) if a.req == 900 => return a.reply,
            _ => continue,
        }
    }
}

#[tokio::test]
async fn full_visibility_shows_every_field_but_not_the_log() {
    let h = Harness::start("[visibility]\nothers_orders = \"full\"\n");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&order("j1", req(1024, 2, 60))).await;
    alex.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    r.send(&chunk("j1", 2, "private output\n")).await;
    r.acked(2).await;
    let mut bob = h.local_hub("bob").await;
    let ReadReply::Job(d) = bob.read(show("j1")).await else {
        panic!("full shows another client's run")
    };
    let s = &d.summary;
    assert_eq!(s.job, JobId("j1".into()));
    assert_eq!(s.state, JobState::Running);
    assert_eq!(s.platform.as_deref(), Some("linux"));
    assert_eq!(s.runner.as_deref(), Some("r1"));
    assert_eq!(s.project.as_deref(), Some("owner/repo"));
    assert_eq!(s.label.as_deref(), Some("agent-1"));
    assert_eq!(s.profile.as_deref(), Some("unit"));
    assert_eq!(s.source, Some(WireSource::Session));
    assert_eq!(d.note.as_deref(), Some("why"));
    assert_eq!(d.branch.as_deref(), Some("main"));
    assert_eq!(d.cwd.as_deref(), Some("/work/repo"));
    assert_eq!(d.os_user.as_deref(), Some("alex"));
    assert_eq!(d.commit.as_deref(), Some("abc"));
    assert_eq!(
        d.steps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["build"]
    );
    match log_reply(&mut bob, "j1").await {
        ReadReply::Denied { .. } => {}
        other => panic!("the log stays with the owner of the run: {other:?}"),
    }
}

#[tokio::test]
async fn history_row_names_key_machine_id_and_label_of_the_client() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut ci = h.member_hub("ci-box", "ci", "10.0.0.20").await;
    let HubToBroker::Order(mut o) = order("c1", req(512, 1, 60)) else {
        unreachable!()
    };
    o.spec.identity.label = "forged-label".into();
    ci.send(&HubToBroker::Order(o)).await;
    ci.accepted().await;
    r.assign().await;
    r.send(&started("c1", 1)).await;
    r.send(&result("c1", 2, JobState::Passed, 0)).await;
    r.acked(2).await;
    let row = h.until_state("ci-box", "c1", JobState::Passed).await.job;
    assert_eq!(
        (
            row.key.client.as_str(),
            row.client_machine.as_str(),
            row.client_label.as_str()
        ),
        ("ci-box", "mid-ci-box", "ci")
    );
    assert_eq!(row.label, "forged-label", "--as stays a declared field");
    let mut bob = h.local_hub("bob").await;
    let jobs = history(
        bob.read(ReadRequest::History(HistoryFilter::default()))
            .await,
    );
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].job, JobId("c1".into()));
    assert_eq!(jobs[0].client_label, "ci");
}
