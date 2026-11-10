mod common;

use common::harness::*;
use common::listeners::FailingListener;
use lr_abstractions::Level;
use lr_broker::{MAX_SUBSCRIPTIONS, OUT_QUEUE_FRAMES};
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
