mod common;

use common::harness::*;
use lr_core::JobId;
use lr_proto::*;

fn withdraw(job: &str) -> HubToBroker {
    HubToBroker::Withdraw(Withdraw {
        job: JobId(job.into()),
    })
}

async fn returned(hub: &mut Conn) -> Returned {
    loop {
        if let BrokerToHub::Returned(r) = hub.hub().await {
            return r;
        }
    }
}

#[tokio::test]
async fn a_queued_order_goes_back_to_the_hub_and_never_runs_here() {
    let h = Harness::start("");
    let mut r = h.local_runner("alex", "lr1", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("local:lr1", true).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    assert_eq!(r.assign().await.job.0, "j1");
    hub.send(&order("j2", req(512, 1, 60))).await;
    hub.accepted().await;
    hub.send(&withdraw("j2")).await;
    let back = returned(&mut hub).await;
    assert_eq!(back.job.0, "j2");
    assert_eq!(back.code, ReasonCode::Moved);
    r.send(&started("j1", 1)).await;
    r.send(&result("j1", 2, lr_core::JobState::Passed, 0)).await;
    hub.send(&order("j3", req(512, 1, 60))).await;
    hub.accepted().await;
    assert_eq!(r.assign().await.job.0, "j3");
}

#[tokio::test]
async fn an_order_that_is_already_assigned_stays_and_is_not_returned() {
    let h = Harness::start("");
    let mut r = h.local_runner("alex", "lr1", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("local:lr1", true).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    assert_eq!(r.assign().await.job.0, "j1");
    hub.send(&withdraw("j1")).await;
    hub.sync().await;
    r.send(&started("j1", 1)).await;
    r.send(&result("j1", 2, lr_core::JobState::Passed, 0)).await;
    loop {
        match hub.hub().await {
            BrokerToHub::Returned(x) => panic!("returned {x:?}"),
            BrokerToHub::JobResult(x) => {
                assert_eq!(x.job.0, "j1");
                break;
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_withdraw_of_an_unknown_job_is_ignored() {
    let h = Harness::start("");
    let _r = h.local_runner("alex", "lr1", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("local:lr1", true).await;
    hub.send(&withdraw("ghost")).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
}

#[tokio::test]
async fn a_returned_order_is_closed_as_moved_in_the_history_of_this_broker() {
    let h = Harness::start("");
    let mut r = h.local_runner("alex", "lr1", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("local:lr1", true).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    assert_eq!(r.assign().await.job.0, "j1");
    hub.send(&order("j2", req(512, 1, 60))).await;
    hub.accepted().await;
    hub.send(&withdraw("j2")).await;
    returned(&mut hub).await;
    let ReadReply::History { jobs, .. } = hub
        .read(ReadRequest::History(HistoryFilter::default()))
        .await
    else {
        panic!("history")
    };
    let j2 = jobs
        .iter()
        .find(|j| j.job.0 == "j2")
        .expect("j2 in history");
    assert_eq!(j2.state, lr_core::JobState::Cancelled);
    assert_eq!(j2.cancelled_by.as_deref(), Some("broker"));
}

#[tokio::test]
async fn an_order_placed_again_after_it_was_returned_runs_here_with_a_stream_of_its_own() {
    let h = Harness::start("");
    let mut r = h.local_runner("alex", "lr1", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("local:lr1", true).await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    assert_eq!(r.assign().await.job.0, "j1");
    hub.send(&order("j2", req(512, 1, 60))).await;
    hub.accepted().await;
    hub.send(&withdraw("j2")).await;
    returned(&mut hub).await;
    r.send(&started("j1", 1)).await;
    let first = hub.started_notice().await;
    assert_eq!(first.job.0, "j1");
    r.send(&result("j1", 2, lr_core::JobState::Passed, 0)).await;
    hub.send(&order("j2", req(512, 1, 60))).await;
    hub.accepted().await;
    assert_eq!(r.assign().await.job.0, "j2");
    r.send(&started("j2", 1)).await;
    loop {
        let s = hub.started_notice().await;
        if s.job.0 == "j2" {
            assert_eq!(s.seq, first.seq);
            break;
        }
    }
}
