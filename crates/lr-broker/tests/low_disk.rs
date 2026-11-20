mod common;

use common::harness::*;
use lr_proto::*;

fn disk(free_disk_mb: Option<u64>) -> RunnerToBroker {
    RunnerToBroker::Heartbeat {
        load: Some(WireLoad {
            free_mem_mb: 16384,
            cpu_busy_percent: 0,
            cpu_cores: 8,
            free_disk_mb,
        }),
    }
}

fn guarded(min_free_disk_mb: u64) -> WireCapabilities {
    WireCapabilities {
        accept_min_free_disk_mb: Some(min_free_disk_mb),
        ..linux()
    }
}

fn status(reply: ReadReply) -> StatusView {
    match reply {
        ReadReply::Status(s) => s,
        other => panic!("expected status, got {other:?}"),
    }
}

#[tokio::test]
async fn an_order_waits_for_free_disk_with_the_figures_and_starts_when_it_appears() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", guarded(40_960)).await;
    let mut alex = h.local_hub("alex").await;
    r.send(&disk(Some(12_288))).await;
    alex.send(&order("j1", req(1024, 1, 60))).await;
    alex.accepted().await;
    r.quiet().await;
    let s = status(
        alex.read(ReadRequest::Status(StatusFilter::default()))
            .await,
    );
    assert_eq!(
        s.queue[0].wait_reason.as_deref(),
        Some("low disk: 12 GB free of 40 GB needed")
    );
    assert_eq!(s.queue[0].wait_code.as_deref(), Some("low_disk"));
    r.send(&disk(Some(50_000))).await;
    r.assign().await;
}

#[tokio::test]
async fn the_larger_of_the_floor_and_the_job_disk_counts() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", guarded(1024)).await;
    let mut alex = h.local_hub("alex").await;
    r.send(&disk(Some(5_000))).await;
    let mut big = req(1024, 1, 60);
    big.disk_mb = 8_000;
    alex.send(&order("j1", big)).await;
    alex.accepted().await;
    r.quiet().await;
    let s = status(
        alex.read(ReadRequest::Status(StatusFilter::default()))
            .await,
    );
    assert_eq!(s.queue[0].wait_code.as_deref(), Some("low_disk"));
    r.send(&disk(Some(9_000))).await;
    r.assign().await;
}

#[tokio::test]
async fn without_a_floor_or_without_a_reading_nothing_is_held() {
    let h = Harness::start("");
    let mut plain = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    plain.send(&disk(Some(1))).await;
    alex.send(&order("j1", req(1024, 1, 60))).await;
    alex.accepted().await;
    plain.assign().await;

    let h = Harness::start("");
    let mut silent = h.member_runner("r1", "10.0.0.7", guarded(40_960)).await;
    let mut alex = h.local_hub("alex").await;
    silent.send(&disk(None)).await;
    alex.send(&order("j1", req(1024, 1, 60))).await;
    alex.accepted().await;
    silent.assign().await;
}

#[tokio::test]
async fn status_shows_the_free_disk_of_each_runner() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", guarded(1024)).await;
    let mut alex = h.local_hub("alex").await;
    r.send(&disk(Some(12_288))).await;
    r.quiet().await;
    let runners = match alex.read(ReadRequest::Runners).await {
        ReadReply::Runners { runners, .. } => runners,
        other => panic!("{other:?}"),
    };
    assert_eq!(runners[0].caps.disk_mb, 12_288);
}
