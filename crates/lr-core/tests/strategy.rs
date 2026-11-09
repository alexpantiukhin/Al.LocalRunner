mod common;

use common::{entry, policy, MIN};
use lr_core::{Condition, Policy, QueueEntry, Rule, RulePolicy, ServeLog, Strategy, Timestamp};

fn ids(queue: &[QueueEntry], p: &RulePolicy, served: &ServeLog, now: u64) -> Vec<String> {
    p.order(queue, served, Timestamp(now))
        .into_iter()
        .map(|i| queue[i].order.job.0.clone())
        .collect()
}

fn with(strategy: Strategy) -> RulePolicy {
    let mut p = policy(8192, 8);
    p.strategy = strategy;
    p
}

#[test]
fn fair_share_alternates_groups_inside_a_priority() {
    let p = with(Strategy::FairShare);
    let queue = [
        entry("a1", "alpha", 1),
        entry("a2", "alpha", 2),
        entry("a3", "alpha", 3),
        entry("b1", "beta", 4),
        entry("b2", "beta", 5),
    ];
    assert_eq!(
        ids(&queue, &p, &ServeLog::default(), 10),
        ["a1", "b1", "a2", "b2", "a3"]
    );
}

#[test]
fn fair_share_starts_with_the_group_longest_without_resources() {
    let p = with(Strategy::FairShare);
    let mut served = ServeLog::default();
    served.record("alpha", Timestamp(9));
    served.record("beta", Timestamp(5));
    let queue = [entry("a1", "alpha", 1), entry("b1", "beta", 2)];
    assert_eq!(ids(&queue, &p, &served, 10), ["b1", "a1"]);
    let queue = [entry("a1", "alpha", 1), entry("g1", "gamma", 2)];
    assert_eq!(ids(&queue, &p, &served, 10), ["g1", "a1"]);
}

#[test]
fn fair_share_keeps_priority_first() {
    let p = with(Strategy::FairShare);
    let queue = [
        entry("a1", "alpha", 1),
        entry("a2", "alpha", 2),
        entry("c1", "ci", 3),
        entry("c2", "ci", 4),
    ];
    assert_eq!(
        ids(&queue, &p, &ServeLog::default(), 10),
        ["c1", "c2", "a1", "a2"]
    );
}

#[test]
fn weighted_share_is_proportional_to_weight() {
    let mut p = with(Strategy::Weighted);
    p.rules.push(Rule {
        weight: Some(2),
        when: Condition {
            client_label: Some("alpha".into()),
            ..Condition::default()
        },
        ..Rule::default()
    });
    let queue: Vec<QueueEntry> = (0..6)
        .map(|i| entry(&format!("a{i}"), "alpha", i))
        .chain((0..6).map(|i| entry(&format!("b{i}"), "beta", 100 + i)))
        .collect();
    let order = ids(&queue, &p, &ServeLog::default(), 1_000);
    let first_six = &order[..6];
    let alpha = first_six.iter().filter(|j| j.starts_with('a')).count();
    assert_eq!(alpha, 4, "{order:?}");
}

#[test]
fn weighted_counts_what_groups_already_received() {
    let p = with(Strategy::Weighted);
    let mut served = ServeLog::default();
    for t in 0..3 {
        served.record("alpha", Timestamp(t));
    }
    let queue = [
        entry("a1", "alpha", 1),
        entry("a2", "alpha", 2),
        entry("b1", "beta", 3),
        entry("b2", "beta", 4),
        entry("b3", "beta", 5),
    ];
    assert_eq!(ids(&queue, &p, &served, 10), ["b1", "b2", "b3", "a1", "a2"]);
}

#[test]
fn aging_applies_on_top_of_fair_share() {
    let p = with(Strategy::FairShare);
    let queue = [entry("c1", "ci", 20 * MIN), entry("old", "alpha", 0)];
    assert_eq!(
        ids(&queue, &p, &ServeLog::default(), 31 * MIN),
        ["old", "c1"]
    );
}

#[test]
fn priority_fifo_ignores_serve_log() {
    let p = with(Strategy::PriorityFifo);
    let mut served = ServeLog::default();
    served.record("alpha", Timestamp(9));
    let queue = [entry("a1", "alpha", 1), entry("b1", "beta", 2)];
    assert_eq!(ids(&queue, &p, &served, 10), ["a1", "b1"]);
}

#[test]
fn serve_log_records_usage() {
    let mut served = ServeLog::default();
    assert_eq!(served.usage("x").granted, 0);
    served.record("x", Timestamp(7));
    served.record("x", Timestamp(9));
    let u = served.usage("x");
    assert_eq!((u.granted, u.last_granted), (2, Some(Timestamp(9))));
}
