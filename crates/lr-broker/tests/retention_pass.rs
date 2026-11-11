mod common;

use common::faulty::FaultyStore;
use common::harness::*;
use lr_abstractions::{Level, PruneLimits, Record};
use lr_broker::RETENTION_EVERY_S;
use lr_core::Timestamp;
use std::time::Duration;

const HOUR_MS: u64 = 3_600_000;
const DAY_MS: u64 = 24 * HOUR_MS;
const GIB: u64 = 1024 * 1024 * 1024;

async fn eventually<F: FnMut() -> bool>(what: &str, mut f: F) {
    for _ in 0..2000 {
        if f() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("never happened: {what}");
}

async fn settle() {
    for _ in 0..2000 {
        tokio::task::yield_now().await;
    }
}

fn prunes(h: &Harness) -> Vec<PruneLimits> {
    h.faults.prunes.lock().unwrap().clone()
}

fn expires(h: &Harness) -> Vec<Timestamp> {
    h.faults.expires.lock().unwrap().clone()
}

fn messages(h: &Harness, message: &str) -> Vec<Record> {
    h.log
        .records()
        .into_iter()
        .filter(|r| r.message == message)
        .collect()
}

async fn passes_after_poke(h: &Harness, n: usize) {
    h.poke().await;
    eventually(&format!("{n} retention passes"), || prunes(h).len() == n).await;
}

#[tokio::test]
async fn the_first_command_runs_a_pass_and_no_second_one_comes_within_ten_minutes() {
    let h = Harness::start("");
    passes_after_poke(&h, 1).await;
    assert_eq!(prunes(&h)[0].now, Timestamp(START));

    h.clock.advance(Duration::from_secs(RETENTION_EVERY_S - 1));
    h.poke().await;
    settle().await;
    assert_eq!(prunes(&h).len(), 1, "a pass within 10 minutes");
    assert_eq!(RETENTION_EVERY_S, 600);

    h.clock.advance(Duration::from_secs(1));
    passes_after_poke(&h, 2).await;
    assert_eq!(prunes(&h)[1].now, Timestamp(START + 600_000));
}

#[tokio::test]
async fn a_pass_still_running_is_not_joined_by_a_second_one() {
    let h = Harness::start("");
    FaultyStore::set(&h.faults.hold_prune);
    passes_after_poke(&h, 1).await;

    h.clock.advance(Duration::from_secs(RETENTION_EVERY_S + 60));
    h.poke().await;
    settle().await;
    assert_eq!(prunes(&h).len(), 1, "a second pass while one runs");

    FaultyStore::clear(&h.faults.hold_prune);
    h.faults.prune_release.notify_one();
    eventually("the held pass is logged", || {
        !messages(&h, "storage cleaned up").is_empty()
    })
    .await;
    passes_after_poke(&h, 2).await;
}

#[tokio::test]
async fn a_store_error_is_a_warning_and_the_next_pass_retries() {
    let h = Harness::start("");
    FaultyStore::set(&h.faults.fail_prune);
    passes_after_poke(&h, 1).await;
    eventually("the failure is logged", || {
        !messages(&h, "storage failed: history was not pruned").is_empty()
    })
    .await;
    let failed = messages(&h, "storage failed: history was not pruned");
    assert_eq!(failed[0].level, Level::Warn);
    assert!(messages(&h, "storage cleaned up").is_empty());

    FaultyStore::clear(&h.faults.fail_prune);
    h.clock.advance(Duration::from_secs(RETENTION_EVERY_S));
    passes_after_poke(&h, 2).await;
    eventually("the retried pass is logged", || {
        messages(&h, "storage cleaned up").len() == 1
    })
    .await;
    let done = &messages(&h, "storage cleaned up")[0];
    assert_eq!(done.level, Level::Info);
    let names: Vec<&str> = done.fields.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "results_expired",
            "logs_removed",
            "rows_removed",
            "bytes_freed"
        ]
    );
}

#[tokio::test]
async fn the_next_pass_takes_its_limits_from_a_reloaded_policy() {
    let h = Harness::start("");
    passes_after_poke(&h, 1).await;
    let before = prunes(&h)[0];

    h.handle
        .reload_policy(&policy(
            "[history]\nkeep_days = 3\nrows_keep_days = 5\nlogs_max_gb = 2\n\
             [results]\nkeep_hours = 7\n",
        ))
        .await
        .expect("reload applies");
    h.clock.advance(Duration::from_secs(RETENTION_EVERY_S));
    passes_after_poke(&h, 2).await;

    let after = prunes(&h)[1];
    let now = START + RETENTION_EVERY_S * 1000;
    assert_eq!(
        after,
        PruneLimits {
            now: Timestamp(now),
            log_max_age_ms: 3 * DAY_MS,
            log_max_bytes: 2 * GIB,
            row_max_age_ms: 5 * DAY_MS,
        }
    );
    assert_ne!(
        (
            before.log_max_age_ms,
            before.row_max_age_ms,
            before.log_max_bytes
        ),
        (
            after.log_max_age_ms,
            after.row_max_age_ms,
            after.log_max_bytes
        )
    );
    assert_eq!(expires(&h)[1], Timestamp(now - 7 * HOUR_MS));
    assert_ne!(expires(&h)[0], Timestamp(START - 7 * HOUR_MS));
}
