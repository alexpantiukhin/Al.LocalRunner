//! Contract cases of [`BrokerStore`]: history queries, statistics and retention.

use super::fixtures::{at, intake};
use crate::store::{
    BrokerStore, JobFilter, JobKey, PruneLimits, StatsQuery, StepUpdate, Transition,
};
use lr_core::{JobState, StepState, Timestamp};

const DAY_MS: u64 = 86_400_000;

async fn finished(
    store: &dyn BrokerStore,
    client: &str,
    job: &str,
    profile: &str,
    times: (u64, u64, u64),
    end: JobState,
) -> JobKey {
    let i = intake(client, job, profile, at(times.0));
    let key = i.job.key.clone();
    store.accept(i).await.unwrap();
    store.grant(&key, "r1").await.unwrap();
    store
        .transition(&key, Transition::to(JobState::Running, at(times.1)))
        .await
        .unwrap();
    store
        .update_step(
            &key,
            StepUpdate {
                ord: 0,
                state: if end == JobState::Passed {
                    StepState::Passed
                } else {
                    StepState::Failed
                },
                exit_code: Some(0),
                started_at: Some(at(times.1)),
                finished_at: Some(at(times.2)),
            },
        )
        .await
        .unwrap();
    if end == JobState::Cancelled {
        store
            .transition(&key, Transition::to(JobState::Cancelling, at(times.2)))
            .await
            .unwrap();
    }
    store
        .transition(&key, Transition::to(end, at(times.2)))
        .await
        .unwrap();
    key
}

/// History lists runs newest first, filtered by every set field and capped by `limit`.
pub async fn history_is_newest_first_filtered_and_limited(store: &dyn BrokerStore) {
    finished(store, "c1", "a", "check", (1, 2, 3), JobState::Passed).await;
    finished(store, "c2", "b", "check", (5, 6, 7), JobState::Failed).await;
    store
        .accept(intake("c1", "c", "lint", at(9)))
        .await
        .unwrap();
    let all = store.jobs(&JobFilter::default()).await.unwrap();
    let ids: Vec<&str> = all.iter().map(|r| r.job.key.job.0.as_str()).collect();
    assert_eq!(ids, vec!["c", "b", "a"]);
    let mine = JobFilter {
        client: Some("c1".into()),
        ..JobFilter::default()
    };
    assert_eq!(store.jobs(&mine).await.unwrap().len(), 2);
    let failed = JobFilter {
        state: Some(JobState::Failed),
        ..JobFilter::default()
    };
    assert_eq!(store.jobs(&failed).await.unwrap()[0].job.key.job.0, "b");
    let since = JobFilter {
        since: Some(at(5)),
        profile: Some("check".into()),
        ..JobFilter::default()
    };
    assert_eq!(store.jobs(&since).await.unwrap().len(), 1);
    let active = JobFilter {
        active_only: true,
        ..JobFilter::default()
    };
    assert_eq!(store.jobs(&active).await.unwrap()[0].job.key.job.0, "c");
    let limited = JobFilter {
        limit: Some(1),
        ..JobFilter::default()
    };
    assert_eq!(store.jobs(&limited).await.unwrap().len(), 1);
}

/// Statistics count final runs per profile and client with median and p95 of wait and
/// duration; runs still active are not counted.
pub async fn stats_count_final_runs_with_median_and_p95(store: &dyn BrokerStore) {
    finished(store, "c1", "a", "check", (0, 10, 20), JobState::Passed).await;
    finished(store, "c1", "b", "check", (0, 20, 50), JobState::Passed).await;
    finished(store, "c2", "c", "check", (0, 30, 130), JobState::Failed).await;
    store
        .accept(intake("c1", "d", "check", at(0)))
        .await
        .unwrap();
    let stats = store.stats(&StatsQuery::default()).await.unwrap();
    assert_eq!(stats.profiles.len(), 1);
    let p = &stats.profiles[0];
    assert_eq!((p.runs, p.passed, p.failed), (3, 2, 1));
    assert_eq!(p.wait_ms.median, Some(20_000));
    assert_eq!(p.wait_ms.p95, Some(30_000));
    assert_eq!(p.duration_ms.median, Some(30_000));
    assert_eq!(p.duration_ms.p95, Some(100_000));
    assert_eq!(p.steps[0].name, "build");
    assert_eq!((p.steps[0].runs, p.steps[0].passed), (3, 2));
    assert_eq!(stats.clients.len(), 2);
    assert_eq!(stats.clients[0].client, "c1");
    assert_eq!(stats.clients[0].busy_ms, 40_000);
    let mine = StatsQuery {
        client: Some("c2".into()),
        ..StatsQuery::default()
    };
    assert_eq!(store.stats(&mine).await.unwrap().profiles[0].runs, 1);
}

/// Stats count timed out, cancelled and lost runs of a profile apart from passed and failed.
pub async fn stats_count_timeouts_cancels_and_losses(store: &dyn BrokerStore) {
    finished(store, "c1", "a", "check", (0, 10, 20), JobState::Passed).await;
    finished(store, "c1", "b", "check", (0, 10, 70), JobState::Timeout).await;
    finished(store, "c1", "c", "check", (0, 10, 30), JobState::Timeout).await;
    finished(store, "c2", "d", "check", (0, 10, 15), JobState::Cancelled).await;
    finished(store, "c2", "e", "check", (0, 10, 40), JobState::Lost).await;
    let stats = store.stats(&StatsQuery::default()).await.unwrap();
    let p = &stats.profiles[0];
    assert_eq!(
        (p.runs, p.passed, p.failed, p.timeouts, p.cancelled, p.lost),
        (5, 1, 0, 2, 1, 1)
    );
    assert_eq!(stats.clients[0].busy_ms, 90_000);
    assert_eq!(stats.clients[1].busy_ms, 35_000);
}

async fn with_log(store: &dyn BrokerStore, key: &JobKey, bytes: usize) {
    store
        .append_log(key, &vec![b'x'; bytes], at(1), u64::MAX)
        .await
        .unwrap();
}

fn limits(now: Timestamp) -> PruneLimits {
    PruneLimits {
        now,
        log_max_age_ms: 30 * DAY_MS,
        log_max_bytes: 1_000_000,
        row_max_age_ms: 365 * DAY_MS,
    }
}

/// Rows past their age go with their logs; logs past theirs go alone.
pub async fn prune_removes_rows_and_logs_by_age(store: &dyn BrokerStore) {
    let old = finished(store, "c1", "old", "p", (0, 1, 2), JobState::Passed).await;
    with_log(store, &old, 10).await;
    let mid_finish = 300 * 86_400;
    let mid = finished(
        store,
        "c1",
        "mid",
        "p",
        (0, 1, mid_finish),
        JobState::Passed,
    )
    .await;
    with_log(store, &mid, 20).await;
    let now = Timestamp(at(2).0 + 366 * DAY_MS);
    let report = store.prune(limits(now)).await.unwrap();
    assert_eq!(report.rows_removed, 1);
    assert_eq!(report.logs_removed, 2);
    assert_eq!(report.bytes_freed, 30);
    assert!(store.job(&old).await.unwrap().is_none());
    let kept = store.job(&mid).await.unwrap().expect("row kept");
    assert!(kept.log.is_none());
    assert!(store.read_log(&mid).await.unwrap().is_empty());
}

/// Over the total log size, logs of the oldest finished runs go first.
pub async fn prune_by_total_log_size_removes_oldest_first(store: &dyn BrokerStore) {
    let a = finished(store, "c1", "a", "p", (0, 1, 2), JobState::Passed).await;
    let b = finished(store, "c1", "b", "p", (0, 1, 3), JobState::Passed).await;
    let c = finished(store, "c1", "c", "p", (0, 1, 4), JobState::Passed).await;
    for key in [&a, &b, &c] {
        with_log(store, key, 400_000).await;
    }
    let report = store.prune(limits(at(10))).await.unwrap();
    assert_eq!(report.logs_removed, 1);
    assert!(store.job(&a).await.unwrap().unwrap().log.is_none());
    assert!(store.job(&b).await.unwrap().unwrap().log.is_some());
    assert_eq!(report.rows_removed, 0);
}

/// Pruning never touches a run that is not final, however old or big.
pub async fn prune_never_touches_active_runs(store: &dyn BrokerStore) {
    let i = intake("c1", "live", "p", at(0));
    let key = i.job.key.clone();
    store.accept(i).await.unwrap();
    store.grant(&key, "r1").await.unwrap();
    store
        .transition(&key, Transition::to(JobState::Running, at(1)))
        .await
        .unwrap();
    with_log(store, &key, 2_000_000).await;
    let now = Timestamp(at(0).0 + 400 * DAY_MS);
    let report = store.prune(limits(now)).await.unwrap();
    assert_eq!(report.logs_removed, 0);
    assert_eq!(report.rows_removed, 0);
    assert_eq!(store.read_log(&key).await.unwrap().len(), 2_000_000);
}
