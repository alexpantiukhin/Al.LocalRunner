mod common;

use common::harness::*;
use common::listeners::FailingListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

async fn settle() {
    for _ in 0..500 {
        tokio::task::yield_now().await;
    }
}

fn retries(h: &Harness) -> Vec<u64> {
    h.log
        .records()
        .into_iter()
        .filter(|r| r.message == "accepting connections fails; retrying later")
        .map(|r| {
            r.fields
                .iter()
                .find(|(k, _)| k == "retry_ms")
                .map(|(_, v)| v.parse().unwrap())
                .unwrap()
        })
        .collect()
}

#[tokio::test]
async fn accept_backoff_starts_after_three_errors_at_100_ms_and_doubles_up_to_5_s() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let h = Harness::build("", &["root"], move |_| {
        Arc::new(FailingListener { calls: c })
    });
    settle().await;
    assert_eq!(calls.load(Ordering::SeqCst), 3, "two retries at once");
    assert_eq!(retries(&h), vec![100]);

    let expected = [100u64, 200, 400, 800, 1600, 3200, 5000, 5000];
    for (i, pause) in expected.iter().enumerate() {
        h.clock.advance(Duration::from_millis(pause - 1));
        settle().await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3 + i,
            "retried before {pause} ms"
        );
        h.clock.advance(Duration::from_millis(1));
        settle().await;
        assert_eq!(calls.load(Ordering::SeqCst), 4 + i, "after {pause} ms");
    }
    let mut seen = expected.to_vec();
    seen.push(5000);
    assert_eq!(retries(&h), seen);
}
