mod common;

use common::{clock, settle, T0};
use lr_core::Timestamp;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn manual_clock_time_moves_only_on_advance() {
    let (manual, clock) = clock();
    assert_eq!(clock.now(), T0);
    settle().await;
    assert_eq!(clock.now(), T0);
    manual.advance(Duration::from_millis(1500));
    assert_eq!(clock.now(), Timestamp(T0.0 + 1500));
}

#[tokio::test]
async fn sleep_deadline_is_fixed_when_sleep_is_called() {
    let (manual, clock) = clock();
    let sleep = clock.sleep(Duration::from_secs(3));
    manual.advance(Duration::from_secs(3));
    sleep.await;
    assert_eq!(clock.now(), T0.plus_secs(3));
}

#[tokio::test]
async fn zero_sleep_and_past_deadline_are_ready_at_once() {
    let (manual, clock) = clock();
    clock.sleep(Duration::ZERO).await;
    manual.advance(Duration::from_secs(10));
    clock.sleep_until(T0.plus_secs(5)).await;
    assert_eq!(manual.sleepers(), 0);
}

#[tokio::test]
async fn advance_wakes_only_sleepers_whose_deadline_passed() {
    let (manual, clock) = clock();
    let woke = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for secs in [1u64, 5, 10] {
        let (c, w) = (clock.clone(), woke.clone());
        tasks.push(tokio::spawn(async move {
            c.sleep(Duration::from_secs(secs)).await;
            w.fetch_add(1, Ordering::SeqCst);
        }));
    }
    manual.wait_for_sleepers(3).await;
    manual.advance(Duration::from_secs(5));
    settle().await;
    assert_eq!(woke.load(Ordering::SeqCst), 2);
    assert_eq!(manual.sleepers(), 1);
    manual.advance(Duration::from_secs(5));
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(woke.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn dropped_sleep_is_not_counted_as_sleeper() {
    let (manual, clock) = clock();
    let c = clock.clone();
    let task = tokio::spawn(async move { c.sleep(Duration::from_secs(60)).await });
    manual.wait_for_sleepers(1).await;
    task.abort();
    let _ = task.await;
    assert_eq!(manual.sleepers(), 0);
}

#[tokio::test]
async fn sleep_until_waits_for_absolute_time() {
    let (manual, clock) = clock();
    let c = clock.clone();
    let task = tokio::spawn(async move { c.sleep_until(T0.plus_secs(30)).await });
    manual.wait_for_sleepers(1).await;
    manual.advance(Duration::from_secs(29));
    settle().await;
    assert!(!task.is_finished());
    manual.advance(Duration::from_secs(1));
    task.await.unwrap();
}
