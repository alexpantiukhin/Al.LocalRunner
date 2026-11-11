mod common;

use common::harness::*;
use lr_core::JobId;
use lr_proto::*;

const NONE: &str = "[visibility]\nothers_orders = \"none\"\n";

fn status(reply: ReadReply) -> StatusView {
    match reply {
        ReadReply::Status(s) => s,
        other => panic!("expected status, got {other:?}"),
    }
}

fn held(reply: ReadReply, runner: &str) -> Vec<HeldTag> {
    let ReadReply::Runners { runners } = reply else {
        panic!("expected runners, got {reply:?}")
    };
    runners
        .into_iter()
        .find(|r| r.name == runner)
        .expect("runner listed")
        .held_tags
}

#[tokio::test]
async fn hidden_level_shows_busy_tags_without_job_id() {
    let h = Harness::start(NONE);
    let mut gpu = linux();
    gpu.tags.insert("gpu".into());
    let mut r = h.member_runner("r1", "10.0.0.7", gpu).await;
    let mut alex = h.local_hub("alex").await;
    let mut want = req(1024, 2, 60);
    want.tags.insert("gpu".into());
    alex.send(&order("j1", want)).await;
    alex.accepted().await;
    r.assign().await;

    let mut bob = h.local_hub("bob").await;
    let seen_by_bob = held(bob.read(ReadRequest::Runners).await, "r1");
    assert_eq!(seen_by_bob.len(), 1);
    assert_eq!(seen_by_bob[0].tag, "gpu");
    assert_eq!(seen_by_bob[0].job, JobId(String::new()));

    let seen_by_alex = held(alex.read(ReadRequest::Runners).await, "r1");
    assert_eq!(seen_by_alex.len(), 1);
    assert_eq!(seen_by_alex[0].tag, "gpu");
    assert_eq!(seen_by_alex[0].job, JobId("j1".into()));
}

#[tokio::test]
async fn queue_position_counts_hidden_orders() {
    let h = Harness::start(NONE);
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&order("a-run", req(1024, 2, 60))).await;
    alex.accepted().await;
    r.assign().await;
    alex.send(&order_at("a-wait1", req(512, 1, 60), START + 1))
        .await;
    alex.accepted().await;
    alex.send(&order_at("a-wait2", req(512, 1, 60), START + 2))
        .await;
    alex.accepted().await;
    let mut bob = h.local_hub("bob").await;
    bob.send(&order_at("b1", req(512, 1, 60), START + 3)).await;
    bob.accepted().await;

    let s = status(bob.read(ReadRequest::Status(StatusFilter::default())).await);
    let queue: Vec<(String, u32)> = s
        .queue
        .iter()
        .map(|q| (q.summary.job.0.clone(), q.position))
        .collect();
    assert_eq!(queue, vec![("b1".to_string(), 3)]);
    assert!(s.running.is_empty(), "alex's run is hidden from bob");
    assert_eq!((s.pool.used_mem_mb, s.pool.used_cpu), (1024, 2));

    let s = status(
        alex.read(ReadRequest::Status(StatusFilter::default()))
            .await,
    );
    let queue: Vec<(String, u32)> = s
        .queue
        .iter()
        .map(|q| (q.summary.job.0.clone(), q.position))
        .collect();
    assert_eq!(
        queue,
        vec![("a-wait1".to_string(), 1), ("a-wait2".to_string(), 2)]
    );
}
