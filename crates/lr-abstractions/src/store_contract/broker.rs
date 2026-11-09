//! Contract cases of [`BrokerStore`]: orders, runs, steps, seq and logs.

use super::fixtures::{at, intake, T0};
use crate::store::{
    Admission, Applied, BrokerStore, JobKey, LogAppend, StepUpdate, StoreError, Transition,
    LOG_TRUNCATED_MARK,
};
use lr_core::{JobState, StepState};

async fn queued(store: &dyn BrokerStore, client: &str, job: &str) -> JobKey {
    let i = intake(client, job, "check", T0);
    let key = i.job.key.clone();
    assert_eq!(store.accept(i).await.unwrap(), Admission::Accepted);
    key
}

pub(crate) async fn running(store: &dyn BrokerStore, client: &str, job: &str) -> JobKey {
    let key = queued(store, client, job).await;
    store.grant(&key, "r1").await.unwrap();
    store
        .transition(&key, Transition::to(JobState::Running, at(10)))
        .await
        .unwrap();
    key
}

async fn state(store: &dyn BrokerStore, key: &JobKey) -> JobState {
    store.job(key).await.unwrap().unwrap().job.state
}

/// An accepted order is a queued run with its steps and an order row.
pub async fn accept_creates_queued_run_and_order_row(store: &dyn BrokerStore) {
    let key = queued(store, "c1", "j1").await;
    let record = store.job(&key).await.unwrap().expect("run stored");
    assert_eq!(record.job.state, JobState::Queued);
    assert_eq!(record.steps.len(), 2);
    assert_eq!(record.job.secrets, vec!["registry_auth".to_string()]);
    let orders = store.orders().await.unwrap();
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0].key, key);
}

/// A repeated order of the same client key and JobId changes nothing.
pub async fn duplicate_order_same_client_key_and_job_id_is_noop(store: &dyn BrokerStore) {
    let key = queued(store, "c1", "j1").await;
    let mut again = intake("c1", "j1", "check", at(5));
    again.job.project = "other/project".into();
    assert_eq!(store.accept(again).await.unwrap(), Admission::Duplicate);
    assert_eq!(store.orders().await.unwrap().len(), 1);
    let record = store.job(&key).await.unwrap().unwrap();
    assert_eq!(record.job.project, "owner/repo");
    assert_eq!(record.job.queued_at, T0);
}

/// The same JobId from another client key is another run.
pub async fn same_job_id_from_another_client_is_a_separate_run(store: &dyn BrokerStore) {
    queued(store, "c1", "j1").await;
    queued(store, "c2", "j1").await;
    assert_eq!(store.orders().await.unwrap().len(), 2);
}

/// The queue lists orders oldest `enqueued_at` first.
pub async fn order_queue_is_oldest_first(store: &dyn BrokerStore) {
    store
        .accept(intake("c1", "late", "p", at(50)))
        .await
        .unwrap();
    store
        .accept(intake("c1", "early", "p", at(5)))
        .await
        .unwrap();
    let orders = store.orders().await.unwrap();
    assert_eq!(orders[0].key.job.0, "early");
    assert_eq!(orders[1].key.job.0, "late");
}

/// Grant removes the order row and records the runner (spec 14).
pub async fn order_row_removed_after_grant(store: &dyn BrokerStore) {
    let key = queued(store, "c1", "j1").await;
    store.grant(&key, "r1").await.unwrap();
    assert!(store.orders().await.unwrap().is_empty());
    let record = store.job(&key).await.unwrap().unwrap();
    assert_eq!(record.job.runner.as_deref(), Some("r1"));
    assert_eq!(record.job.state, JobState::Queued);
    assert!(matches!(
        store.grant(&key, "r2").await,
        Err(StoreError::NotFound(_))
    ));
}

/// `Running` needs a grant and takes the broker time as `started_at`.
pub async fn start_requires_grant_and_records_broker_time(store: &dyn BrokerStore) {
    let key = queued(store, "c1", "j1").await;
    assert!(matches!(
        store
            .transition(&key, Transition::to(JobState::Running, at(1)))
            .await,
        Err(StoreError::Conflict(_))
    ));
    store.grant(&key, "r1").await.unwrap();
    store
        .transition(&key, Transition::to(JobState::Running, at(7)))
        .await
        .unwrap();
    let record = store.job(&key).await.unwrap().unwrap();
    assert_eq!(record.job.state, JobState::Running);
    assert_eq!(record.job.started_at, Some(at(7)));
}

/// The final state is written once; a repeated or second final result changes nothing.
pub async fn final_state_written_once(store: &dyn BrokerStore) {
    let key = running(store, "c1", "j1").await;
    let mut passed = Transition::to(JobState::Passed, at(20));
    passed.exit_code = Some(0);
    assert_eq!(
        store.transition(&key, passed.clone()).await.unwrap(),
        Applied::Changed
    );
    assert_eq!(
        store.transition(&key, passed).await.unwrap(),
        Applied::Ignored
    );
    let mut failed = Transition::to(JobState::Failed, at(30));
    failed.exit_code = Some(1);
    assert_eq!(
        store.transition(&key, failed).await.unwrap(),
        Applied::Ignored
    );
    let late_lost = Transition::to(JobState::Lost, at(40));
    assert_eq!(
        store.transition(&key, late_lost).await.unwrap(),
        Applied::Ignored
    );
    let record = store.job(&key).await.unwrap().unwrap();
    assert_eq!(record.job.state, JobState::Passed);
    assert_eq!(record.job.exit_code, Some(0));
    assert_eq!(record.job.finished_at, Some(at(20)));
}

/// Changes the state machine of spec 15.1 does not allow are refused.
pub async fn disallowed_transition_is_refused(store: &dyn BrokerStore) {
    let key = queued(store, "c1", "j1").await;
    assert!(matches!(
        store
            .transition(&key, Transition::to(JobState::Passed, at(1)))
            .await,
        Err(StoreError::InvalidTransition {
            from: JobState::Queued,
            to: JobState::Passed
        })
    ));
    let missing = JobKey::new("c1", "nope");
    assert!(matches!(
        store
            .transition(&missing, Transition::to(JobState::Cancelled, at(1)))
            .await,
        Err(StoreError::NotFound(_))
    ));
}

/// Cancel of a waiting order ends the run at once and removes its order row.
pub async fn cancel_of_waiting_order_removes_it_at_once(store: &dyn BrokerStore) {
    let key = queued(store, "c1", "j1").await;
    let mut cancel = Transition::to(JobState::Cancelled, at(3));
    cancel.cancelled_by = Some("alex".into());
    cancel.cancel_reason = Some("cancelled_by_user".into());
    store.transition(&key, cancel).await.unwrap();
    assert!(store.orders().await.unwrap().is_empty());
    let record = store.job(&key).await.unwrap().unwrap();
    assert_eq!(record.job.state, JobState::Cancelled);
    assert_eq!(record.job.finished_at, Some(at(3)));
    assert_eq!(record.job.cancelled_by.as_deref(), Some("alex"));
}

/// An accepted cancel only moves a running job to `cancelling`; the canceller is kept and the
/// final state comes from the runner report.
pub async fn accepted_cancel_keeps_canceller_until_runner_reports(store: &dyn BrokerStore) {
    let key = running(store, "c1", "j1").await;
    let mut cancel = Transition::to(JobState::Cancelling, at(12));
    cancel.cancelled_by = Some("dashboard:alex".into());
    cancel.cancel_reason = Some("stuck".into());
    store.transition(&key, cancel).await.unwrap();
    assert_eq!(state(store, &key).await, JobState::Cancelling);
    store
        .transition(&key, Transition::to(JobState::Cancelled, at(20)))
        .await
        .unwrap();
    let record = store.job(&key).await.unwrap().unwrap();
    assert_eq!(record.job.state, JobState::Cancelled);
    assert_eq!(record.job.cancelled_by.as_deref(), Some("dashboard:alex"));
    assert_eq!(record.job.cancel_reason.as_deref(), Some("stuck"));
}

/// A run in `cancelling` whose runner vanished ends `lost` (spec 15.5 reasons).
pub async fn cancelling_run_of_vanished_runner_ends_lost(store: &dyn BrokerStore) {
    let key = running(store, "c1", "j1").await;
    store
        .transition(&key, Transition::to(JobState::Cancelling, at(12)))
        .await
        .unwrap();
    let mut lost = Transition::to(JobState::Lost, at(200));
    lost.cancel_reason = Some("runner_lost".into());
    store.transition(&key, lost).await.unwrap();
    let record = store.job(&key).await.unwrap().unwrap();
    assert_eq!(record.job.state, JobState::Lost);
    assert_eq!(record.job.cancel_reason.as_deref(), Some("runner_lost"));
}

/// A granted run that never started goes back to the queue with its runner cleared.
pub async fn unstarted_granted_run_can_be_requeued(store: &dyn BrokerStore) {
    let i = intake("c1", "j1", "check", T0);
    let (key, order) = (i.job.key.clone(), i.order.clone());
    store.accept(i).await.unwrap();
    store.grant(&key, "r1").await.unwrap();
    store.requeue(order.clone()).await.unwrap();
    let orders = store.orders().await.unwrap();
    assert_eq!(orders, vec![order.clone()]);
    assert_eq!(store.job(&key).await.unwrap().unwrap().job.runner, None);
    assert!(matches!(
        store.requeue(order).await,
        Err(StoreError::Duplicate(_))
    ));
}

/// A started run is never queued again, so nothing runs twice.
pub async fn started_run_cannot_be_requeued(store: &dyn BrokerStore) {
    let key = running(store, "c1", "j1").await;
    let order = intake("c1", "j1", "check", T0).order;
    assert!(matches!(
        store.requeue(order).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(store.orders().await.unwrap().is_empty());
    assert_eq!(state(store, &key).await, JobState::Running);
}

/// Step updates apply until the step ended or the run is final.
pub async fn step_updates_stop_at_step_end_and_run_end(store: &dyn BrokerStore) {
    let key = running(store, "c1", "j1").await;
    let update = |state, exit_code, finished_at| StepUpdate {
        ord: 0,
        state,
        exit_code,
        started_at: Some(at(10)),
        finished_at,
    };
    let run = update(StepState::Running, None, None);
    assert_eq!(
        store.update_step(&key, run).await.unwrap(),
        Applied::Changed
    );
    let done = update(StepState::Passed, Some(0), Some(at(15)));
    assert_eq!(
        store.update_step(&key, done).await.unwrap(),
        Applied::Changed
    );
    let again = update(StepState::Failed, Some(1), Some(at(16)));
    assert_eq!(
        store.update_step(&key, again).await.unwrap(),
        Applied::Ignored
    );
    store
        .transition(&key, Transition::to(JobState::Lost, at(30)))
        .await
        .unwrap();
    let late = StepUpdate {
        ord: 1,
        ..update(StepState::Passed, Some(0), Some(at(31)))
    };
    assert_eq!(
        store.update_step(&key, late).await.unwrap(),
        Applied::Ignored
    );
    let steps = store.job(&key).await.unwrap().unwrap().steps;
    assert_eq!(steps[0].state, StepState::Passed);
    assert_eq!(steps[0].finished_at, Some(at(15)));
    assert_eq!(steps[1].state, StepState::Queued);
}

/// A message seq seen before is reported as a repeat, per client key and JobId (spec 3.9).
pub async fn repeated_seq_is_dropped_per_run(store: &dyn BrokerStore) {
    let a = queued(store, "c1", "j1").await;
    let b = queued(store, "c2", "j1").await;
    assert_eq!(store.last_seq(&a).await.unwrap(), 0);
    assert!(store.accept_seq(&a, 1).await.unwrap());
    assert!(!store.accept_seq(&a, 1).await.unwrap());
    assert!(store.accept_seq(&a, 3).await.unwrap());
    assert!(!store.accept_seq(&a, 2).await.unwrap());
    assert_eq!(store.last_seq(&a).await.unwrap(), 3);
    assert!(store.accept_seq(&b, 1).await.unwrap());
    assert!(matches!(
        store.accept_seq(&JobKey::new("c1", "nope"), 1).await,
        Err(StoreError::NotFound(_))
    ));
}

/// Output appended to a run's log is read back whole, with its size recorded.
pub async fn log_is_appended_and_read_back(store: &dyn BrokerStore) {
    let key = running(store, "c1", "j1").await;
    assert!(store.read_log(&key).await.unwrap().is_empty());
    let first = store
        .append_log(&key, b"one\n", at(11), 1024)
        .await
        .unwrap();
    assert_eq!(first, LogAppend::Appended);
    store
        .append_log(&key, b"two\n", at(12), 1024)
        .await
        .unwrap();
    assert_eq!(store.read_log(&key).await.unwrap(), b"one\ntwo\n".to_vec());
    let log = store.job(&key).await.unwrap().unwrap().log.unwrap();
    assert_eq!(log.bytes, 8);
    assert_eq!(log.updated_at, at(12));
}

/// A log reaching its limit is cut with a mark once; later output is dropped (spec 8).
pub async fn log_cut_at_limit_with_mark_then_dropped(store: &dyn BrokerStore) {
    let key = running(store, "c1", "j1").await;
    store.append_log(&key, b"12345", at(11), 8).await.unwrap();
    let cut = store.append_log(&key, b"67890", at(12), 8).await.unwrap();
    assert_eq!(cut, LogAppend::Truncated);
    let after = store.append_log(&key, b"more", at(13), 8).await.unwrap();
    assert_eq!(after, LogAppend::Dropped);
    let mut expected = b"12345678".to_vec();
    expected.extend_from_slice(LOG_TRUNCATED_MARK);
    assert_eq!(store.read_log(&key).await.unwrap(), expected);
}
