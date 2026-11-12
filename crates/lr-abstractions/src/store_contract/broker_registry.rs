//! Contract cases of [`BrokerStore`]: join codes, key records, runners and the result outbox.

use super::fixtures::{at, code, join, machine, runner};
use crate::store::{BrokerStore, MachineKind, OutboxBacklog, OutboxKind, RunnerState, StoreError};
use lr_core::JobId;

async fn joined(store: &dyn BrokerStore, name: &str, kind: MachineKind) {
    let hash = format!("code-{name}");
    store.issue_code(code(&hash, kind, 1)).await.unwrap();
    store
        .redeem(join(
            &hash,
            machine(name, kind, &format!("key-{name}")),
            at(1),
        ))
        .await
        .unwrap();
}

async fn uses_left(store: &dyn BrokerStore, hash: &str) -> u32 {
    let codes = store.join_codes().await.unwrap();
    codes
        .iter()
        .find(|c| c.code_hash == hash)
        .unwrap()
        .uses_left
}

/// A join records the key and spends the single use of the code (spec 3.8, 15.5).
pub async fn join_records_key_and_spends_single_use(store: &dyn BrokerStore) {
    store
        .issue_code(code("h", MachineKind::Hub, 1))
        .await
        .unwrap();
    let m = machine("laptop", MachineKind::Hub, "k1");
    let stored = store.redeem(join("h", m.clone(), at(5))).await.unwrap();
    assert_eq!(stored, m);
    assert_eq!(store.machines().await.unwrap(), vec![m]);
    let c = &store.join_codes().await.unwrap()[0];
    assert_eq!(c.uses_left, 0);
    assert_eq!(c.redeemed_at, Some(at(5)));
    assert_eq!(c.machine_name.as_deref(), Some("laptop"));
    let again = machine("other", MachineKind::Hub, "k2");
    assert_eq!(
        store.redeem(join("h", again, at(6))).await,
        Err(StoreError::CodeUsedUp)
    );
}

/// Issuing a code drops the codes that are used up or expired by then, and keeps open ones.
pub async fn issuing_a_code_drops_used_up_and_expired_codes(store: &dyn BrokerStore) {
    joined(store, "spent", MachineKind::Runner).await;
    let mut old = code("old", MachineKind::Runner, 1);
    old.expires_at = at(10);
    store.issue_code(old).await.unwrap();
    store
        .issue_code(code("open", MachineKind::Runner, 1))
        .await
        .unwrap();
    let mut late = code("late", MachineKind::Hub, 1);
    late.issued_at = at(10);
    store.issue_code(late).await.unwrap();
    let mut hashes: Vec<String> = store
        .join_codes()
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.code_hash)
        .collect();
    hashes.sort();
    assert_eq!(hashes, vec!["late".to_string(), "open".to_string()]);
}

/// A code for N runners counts down one use per join.
pub async fn multi_use_code_counts_down_per_join(store: &dyn BrokerStore) {
    store
        .issue_code(code("h", MachineKind::Runner, 2))
        .await
        .unwrap();
    let first = machine("r-1", MachineKind::Runner, "k1");
    store.redeem(join("h", first, at(1))).await.unwrap();
    assert_eq!(uses_left(store, "h").await, 1);
    assert_eq!(store.join_codes().await.unwrap()[0].redeemed_at, None);
    let second = machine("r-2", MachineKind::Runner, "k2");
    store.redeem(join("h", second, at(2))).await.unwrap();
    assert_eq!(uses_left(store, "h").await, 0);
}

/// A code issued with `--name` admits only that name, and spends nothing on another one.
pub async fn named_code_admits_only_its_name(store: &dyn BrokerStore) {
    let mut c = code("h", MachineKind::Hub, 1);
    c.name = Some("laptop".into());
    store.issue_code(c).await.unwrap();
    let other = machine("desktop", MachineKind::Hub, "k1");
    assert_eq!(
        store.redeem(join("h", other, at(1))).await,
        Err(StoreError::CodeNameMismatch)
    );
    assert_eq!(uses_left(store, "h").await, 1);
    assert!(store.machines().await.unwrap().is_empty());
    let named = machine("laptop", MachineKind::Hub, "k1");
    let stored = store.redeem(join("h", named, at(2))).await.unwrap();
    assert_eq!(stored.name, "laptop");
}

/// The label of the code replaces the label the joining machine brought.
pub async fn code_label_applied_to_joining_machine(store: &dyn BrokerStore) {
    let mut c = code("h", MachineKind::Runner, 2);
    c.label = Some("ci-farm".into());
    store.issue_code(c).await.unwrap();
    let stored = store
        .redeem(join("h", machine("r-1", MachineKind::Runner, "k1"), at(1)))
        .await
        .unwrap();
    assert_eq!(stored.label, "ci-farm");
    let kept = store.machine_by_key("k1").await.unwrap().unwrap();
    assert_eq!(kept.label, "ci-farm");
    let mut unlabelled = code("u", MachineKind::Runner, 1);
    unlabelled.label = None;
    store.issue_code(unlabelled).await.unwrap();
    let own = store
        .redeem(join("u", machine("r-2", MachineKind::Runner, "k2"), at(2)))
        .await
        .unwrap();
    assert_eq!(own.label, "agent");
}

/// A code is found by its hash, with its current uses; an unknown hash finds nothing.
pub async fn join_code_found_by_hash(store: &dyn BrokerStore) {
    let c = code("h", MachineKind::Runner, 2);
    store.issue_code(c.clone()).await.unwrap();
    store
        .issue_code(code("g", MachineKind::Hub, 1))
        .await
        .unwrap();
    assert_eq!(store.join_code("h").await.unwrap(), Some(c));
    store
        .redeem(join("h", machine("r-1", MachineKind::Runner, "k1"), at(1)))
        .await
        .unwrap();
    assert_eq!(store.join_code("h").await.unwrap().unwrap().uses_left, 1);
    assert_eq!(store.join_code("nope").await.unwrap(), None);
}

/// Unknown, expired and other-kind codes are refused and spend nothing.
pub async fn invalid_codes_refused_without_spending_a_use(store: &dyn BrokerStore) {
    store
        .issue_code(code("h", MachineKind::Runner, 1))
        .await
        .unwrap();
    let m = machine("r", MachineKind::Runner, "k");
    assert_eq!(
        store.redeem(join("nope", m.clone(), at(1))).await,
        Err(StoreError::CodeUnknown)
    );
    assert_eq!(
        store.redeem(join("h", m.clone(), at(3600))).await,
        Err(StoreError::CodeExpired)
    );
    let hub = machine("hub", MachineKind::Hub, "k");
    assert_eq!(
        store.redeem(join("h", hub, at(1))).await,
        Err(StoreError::CodeKindMismatch)
    );
    assert_eq!(uses_left(store, "h").await, 1);
    assert!(store.machines().await.unwrap().is_empty());
}

/// A name or key held by an unrevoked machine is refused and the code keeps its use; after
/// revocation the name can join again.
pub async fn taken_name_refused_until_revoked(store: &dyn BrokerStore) {
    joined(store, "laptop", MachineKind::Hub).await;
    store
        .issue_code(code("h2", MachineKind::Hub, 1))
        .await
        .unwrap();
    let same_name = machine("laptop", MachineKind::Hub, "new-key");
    assert!(matches!(
        store.redeem(join("h2", same_name.clone(), at(2))).await,
        Err(StoreError::Duplicate(_))
    ));
    let same_key = machine("desk", MachineKind::Hub, "key-laptop");
    assert!(matches!(
        store.redeem(join("h2", same_key, at(2))).await,
        Err(StoreError::Duplicate(_))
    ));
    assert_eq!(uses_left(store, "h2").await, 1);
    store.revoke("laptop", at(3)).await.unwrap();
    store.redeem(join("h2", same_name, at(4))).await.unwrap();
    let found = store.machine_by_key("new-key").await.unwrap().unwrap();
    assert_eq!(found.revoked_at, None);
}

/// A key record is found by its public key, revoked or not, and keeps its last heartbeat.
pub async fn machine_found_by_key_and_last_seen_updated(store: &dyn BrokerStore) {
    joined(store, "laptop", MachineKind::Hub).await;
    store.touch_machine("laptop", at(42)).await.unwrap();
    let found = store.machine_by_key("key-laptop").await.unwrap().unwrap();
    assert_eq!(found.last_seen, Some(at(42)));
    assert!(store.machine_by_key("unknown").await.unwrap().is_none());
    assert!(matches!(
        store.touch_machine("ghost", at(1)).await,
        Err(StoreError::NotFound(_))
    ));
    store.revoke("laptop", at(50)).await.unwrap();
    let revoked = store.machine_by_key("key-laptop").await.unwrap().unwrap();
    assert_eq!(revoked.revoked_at, Some(at(50)));
}

/// Revocation marks the machine's runners removed and deletes its outbox (spec 3.8).
pub async fn revoke_removes_runners_and_outbox_of_the_key(store: &dyn BrokerStore) {
    joined(store, "box", MachineKind::Runner).await;
    joined(store, "laptop", MachineKind::Hub).await;
    store.upsert_runner(runner("light", "box")).await.unwrap();
    store.upsert_runner(runner("heavy", "box")).await.unwrap();
    let job = JobId("j1".into());
    for client in ["laptop", "box"] {
        store
            .push_result(
                client,
                &job,
                OutboxKind::JobResult,
                "{}".into(),
                at(1),
                1 << 20,
            )
            .await
            .unwrap();
    }
    let revoked = store.revoke("box", at(9)).await.unwrap();
    let mut names = revoked.runners.clone();
    names.sort();
    assert_eq!(names, vec!["heavy".to_string(), "light".to_string()]);
    assert_eq!(revoked.outbox_removed, 1);
    let runners = store.runners().await.unwrap();
    assert!(runners.iter().all(|r| r.state == RunnerState::Removed));
    assert_eq!(store.pending_results("laptop").await.unwrap().len(), 1);
}

/// A runner needs its key record and a name no other machine's active runner holds.
pub async fn runner_needs_machine_and_unique_name(store: &dyn BrokerStore) {
    assert!(matches!(
        store.upsert_runner(runner("light", "ghost")).await,
        Err(StoreError::NotFound(_))
    ));
    joined(store, "box1", MachineKind::Runner).await;
    joined(store, "box2", MachineKind::Runner).await;
    store.upsert_runner(runner("light", "box1")).await.unwrap();
    let mut update = runner("light", "box1");
    update.cache_used_mb = 99;
    store.upsert_runner(update).await.unwrap();
    assert_eq!(store.runners().await.unwrap()[0].cache_used_mb, 99);
    assert!(matches!(
        store.upsert_runner(runner("light", "box2")).await,
        Err(StoreError::Duplicate(_))
    ));
}

/// Runner state and last heartbeat are stored (spec 15.4).
pub async fn runner_state_and_last_seen_stored(store: &dyn BrokerStore) {
    joined(store, "box", MachineKind::Runner).await;
    store.upsert_runner(runner("light", "box")).await.unwrap();
    store
        .set_runner_state("light", RunnerState::Online, at(15))
        .await
        .unwrap();
    let r = &store.runners().await.unwrap()[0];
    assert_eq!(r.state, RunnerState::Online);
    assert_eq!(r.last_seen, Some(at(15)));
    assert!(matches!(
        store
            .set_runner_state("ghost", RunnerState::Online, at(1))
            .await,
        Err(StoreError::NotFound(_))
    ));
}

/// Result seq counts per client and job from 1 and keeps growing after an ack; an ack removes
/// entries up to its seq (spec 15.3).
pub async fn result_outbox_seq_per_client_and_job_and_ack_removes(store: &dyn BrokerStore) {
    let (j1, j2) = (JobId("j1".into()), JobId("j2".into()));
    let push = |client: &'static str, job: JobId| async move {
        store
            .push_result(client, &job, OutboxKind::Chunk, "x".into(), at(1), 1 << 20)
            .await
            .unwrap()
            .unwrap()
    };
    assert_eq!(push("c1", j1.clone()).await, 1);
    assert_eq!(push("c1", j1.clone()).await, 2);
    assert_eq!(push("c1", j2.clone()).await, 1);
    assert_eq!(push("c2", j1.clone()).await, 1);
    assert_eq!(store.ack_results("c1", &j1, 1).await.unwrap(), 1);
    assert_eq!(push("c1", j1.clone()).await, 3);
    let pending = store.pending_results("c1").await.unwrap();
    let seqs: Vec<(String, u64)> = pending.iter().map(|e| (e.job.0.clone(), e.seq)).collect();
    assert_eq!(
        seqs,
        vec![("j1".into(), 2), ("j1".into(), 3), ("j2".into(), 1)]
    );
    assert_eq!(store.ack_results("c1", &j1, 3).await.unwrap(), 2);
    assert_eq!(store.ack_results("c1", &j1, 3).await.unwrap(), 0);
    assert_eq!(store.pending_results("c2").await.unwrap().len(), 1);
}

/// Over a client's outbox limit, output chunks are dropped and results kept (spec 8).
pub async fn chunks_over_limit_dropped_results_kept(store: &dyn BrokerStore) {
    let job = JobId("j1".into());
    let first = store
        .push_result("c1", &job, OutboxKind::Chunk, "12345".into(), at(1), 8)
        .await
        .unwrap();
    assert_eq!(first, Some(1));
    let dropped = store
        .push_result("c1", &job, OutboxKind::Chunk, "67890".into(), at(2), 8)
        .await
        .unwrap();
    assert_eq!(dropped, None);
    let result = store
        .push_result(
            "c1",
            &job,
            OutboxKind::JobResult,
            "{\"r\":1}".into(),
            at(3),
            8,
        )
        .await
        .unwrap();
    assert_eq!(result, Some(2));
    let kinds: Vec<OutboxKind> = store
        .pending_results("c1")
        .await
        .unwrap()
        .iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, vec![OutboxKind::Chunk, OutboxKind::JobResult]);
}

/// Results kept longer than `[results] keep_hours` are removed.
pub async fn expired_results_removed(store: &dyn BrokerStore) {
    let job = JobId("j1".into());
    for t in [1, 2, 10] {
        store
            .push_result("c1", &job, OutboxKind::Chunk, "x".into(), at(t), 1 << 20)
            .await
            .unwrap();
    }
    assert_eq!(store.expire_results(at(3)).await.unwrap(), 2);
    assert_eq!(store.pending_results("c1").await.unwrap()[0].seq, 3);
}

/// A result seq is never used twice for a client and job, whatever removed the entries:
/// an ack, the keep period or a revocation.
pub async fn result_seq_never_reused_after_expire_or_revoke(store: &dyn BrokerStore) {
    let job = JobId("j1".into());
    let push = |client: &'static str, t: u64| async move {
        store
            .push_result(
                client,
                &job_id(),
                OutboxKind::Chunk,
                "x".into(),
                at(t),
                1 << 20,
            )
            .await
            .unwrap()
            .unwrap()
    };
    fn job_id() -> JobId {
        JobId("j1".into())
    }
    assert_eq!(push("c1", 1).await, 1);
    store.ack_results("c1", &job, 1).await.unwrap();
    assert_eq!(store.expire_results(at(100)).await.unwrap(), 0);
    assert_eq!(push("c1", 200).await, 2);
    assert_eq!(store.expire_results(at(300)).await.unwrap(), 1);
    assert_eq!(push("c1", 400).await, 3);
    joined(store, "c1", MachineKind::Hub).await;
    store.revoke("c1", at(500)).await.unwrap();
    assert!(store.pending_results("c1").await.unwrap().is_empty());
    assert_eq!(push("c1", 600).await, 4);
}

/// The backlog counts the unacknowledged results of every client and the oldest of them,
/// without the acknowledged ones (spec 12.2 `outbox_pending`).
pub async fn results_backlog_counts_every_client(store: &dyn BrokerStore) {
    assert_eq!(
        store.results_backlog().await.unwrap(),
        OutboxBacklog::default()
    );
    let job = JobId("j1".into());
    for (client, t) in [("c1", 5), ("c1", 7), ("c2", 3)] {
        store
            .push_result(client, &job, OutboxKind::Chunk, "x".into(), at(t), 1 << 20)
            .await
            .unwrap();
    }
    let all = store.results_backlog().await.unwrap();
    assert_eq!(
        all,
        OutboxBacklog {
            pending: 3,
            oldest: Some(at(3))
        }
    );
    store.ack_results("c2", &job, 1).await.unwrap();
    let left = store.results_backlog().await.unwrap();
    assert_eq!(
        left,
        OutboxBacklog {
            pending: 2,
            oldest: Some(at(5))
        }
    );
}
