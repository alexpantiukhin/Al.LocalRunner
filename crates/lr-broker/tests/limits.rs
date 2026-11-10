mod common;

use common::harness::*;
use common::listeners::FailingListener;
use lr_abstractions::Level;
use lr_broker::{MAX_SUBSCRIPTIONS, OUT_QUEUE_FRAMES};
use lr_core::Requirements;
use lr_proto::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

async fn eventually(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..500 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never happened: {what}");
}

#[tokio::test]
async fn slow_reader_is_disconnected_and_logged() {
    let (h, stall) = Harness::stalling("");
    let slow = h.local_hub("alex").await;
    stall.start();
    for req in 0..(OUT_QUEUE_FRAMES as u64 + 8) {
        slow.send(&HubToBroker::Read(ReadCall {
            req,
            request: ReadRequest::Runners,
        }))
        .await;
    }
    eventually("slow connection closed", || stall.closes() >= 1).await;
    stall.on.store(false, Ordering::SeqCst);
    let mut other = h.local_hub("bob").await;
    other.sync().await;
    let warned = h
        .log
        .records()
        .into_iter()
        .any(|r| r.level == Level::Warn && r.message.contains("slow"));
    assert!(warned, "{:?}", h.log.records());
}

#[tokio::test]
async fn subscriptions_per_connection_are_capped() {
    let h = Harness::start("");
    let mut hub = h.local_hub("alex").await;
    for _ in 0..MAX_SUBSCRIPTIONS {
        let reply = hub
            .read(ReadRequest::Subscribe {
                topics: vec![Topic::Queue],
            })
            .await;
        assert!(matches!(reply, ReadReply::Subscribed { .. }), "{reply:?}");
    }
    match hub
        .read(ReadRequest::Subscribe {
            topics: vec![Topic::Runners],
        })
        .await
    {
        ReadReply::Failed { message } => assert!(message.contains("subscriptions"), "{message}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn unsubscribing_frees_a_subscription_slot() {
    let h = Harness::start("");
    let mut hub = h.local_hub("alex").await;
    let mut last = 0;
    for _ in 0..MAX_SUBSCRIPTIONS {
        if let ReadReply::Subscribed { sub } = hub
            .read(ReadRequest::Subscribe {
                topics: vec![Topic::Queue],
            })
            .await
        {
            last = sub;
        }
    }
    hub.read(ReadRequest::Unsubscribe { sub: last }).await;
    let reply = hub
        .read(ReadRequest::Subscribe {
            topics: vec![Topic::Queue],
        })
        .await;
    assert!(matches!(reply, ReadReply::Subscribed { .. }), "{reply:?}");
}

#[tokio::test]
async fn failing_listener_is_retried_with_backoff_not_a_busy_loop() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let h = Harness::build("", &["root"], move |_| {
        Arc::new(FailingListener { calls: c })
    });
    for _ in 0..2000 {
        tokio::task::yield_now().await;
    }
    let before = calls.load(Ordering::SeqCst);
    assert!(
        before <= 4,
        "accept called {before} times without time passing"
    );
    for _ in 0..10 {
        h.clock.advance(Duration::from_secs(10));
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
    }
    assert!(calls.load(Ordering::SeqCst) > before);
}

fn overridden(job: &str, requirements: Requirements, over: ResourceOverride) -> HubToBroker {
    let HubToBroker::Order(mut o) = order(job, requirements) else {
        unreachable!()
    };
    o.spec.overrides = over;
    HubToBroker::Order(o)
}

#[tokio::test]
async fn resource_override_above_pool_is_rejected_with_its_own_code() {
    let h = Harness::start("");
    let _r = h
        .member_runner("r1", "10.0.0.7", caps(&["linux"], 65536, 32))
        .await;
    let mut hub = h.local_hub("alex").await;
    let mem = ResourceOverride {
        mem_mb: Some(20000),
        cpu: None,
    };
    hub.send(&overridden("j1", req(20000, 1, 60), mem)).await;
    let r = hub.rejected().await;
    assert_eq!(
        r.code,
        ReasonCode::Rejected(RejectReason::OverrideAbovePool)
    );
    assert!(
        r.detail.contains("--mem") && r.detail.contains("20000") && r.detail.contains("16384"),
        "{}",
        r.detail
    );
    let cpu = ResourceOverride {
        mem_mb: None,
        cpu: Some(12),
    };
    hub.send(&overridden("j2", req(512, 12, 60), cpu)).await;
    let r = hub.rejected().await;
    assert_eq!(
        r.code,
        ReasonCode::Rejected(RejectReason::OverrideAbovePool)
    );
    assert!(
        r.detail.contains("--cpu") && r.detail.contains("12") && r.detail.contains('8'),
        "{}",
        r.detail
    );
}

#[tokio::test]
async fn profile_value_above_pool_keeps_the_pool_code_despite_another_override() {
    let h = Harness::start("");
    let _r = h
        .member_runner("r1", "10.0.0.7", caps(&["linux"], 65536, 32))
        .await;
    let mut hub = h.local_hub("alex").await;
    let cpu_only = ResourceOverride {
        mem_mb: None,
        cpu: Some(2),
    };
    hub.send(&overridden("j1", req(20000, 2, 60), cpu_only))
        .await;
    let r = hub.rejected().await;
    assert_eq!(r.code, ReasonCode::Rejected(RejectReason::PoolMemory));
}

#[tokio::test]
async fn resource_override_within_pool_is_accepted() {
    let h = Harness::start("");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    let over = ResourceOverride {
        mem_mb: Some(6144),
        cpu: Some(4),
    };
    hub.send(&overridden("j1", req(6144, 4, 60), over)).await;
    assert_eq!(hub.accepted().await.job.0, "j1");
}
