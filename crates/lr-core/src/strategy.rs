use crate::model::{JobId, Timestamp};
use crate::policy::{Policy, RulePolicy, Strategy};
use crate::queue::{GroupUsage, QueueEntry, ServeLog};
use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, VecDeque};

fn fifo_key(e: &QueueEntry) -> (Timestamp, Timestamp, &JobId) {
    (e.order.enqueued_at, e.accepted_at, &e.order.job)
}

/// Dispatch order of `queue` under `policy` at `now` (spec 3.5, 6): by descending
/// priority; inside a priority by the strategy: enqueue time for
/// `priority-fifo`, groups in turn for `fair-share` and `weighted`.
pub fn dispatch_order(
    policy: &RulePolicy,
    queue: &[QueueEntry],
    served: &ServeLog,
    now: Timestamp,
) -> Vec<usize> {
    let prio: Vec<i64> = queue.iter().map(|e| policy.priority_of(e)).collect();
    let mut idx: Vec<usize> = (0..queue.len()).collect();
    idx.sort_by(|&a, &b| {
        Reverse(prio[a])
            .cmp(&Reverse(prio[b]))
            .then_with(|| fifo_key(&queue[a]).cmp(&fifo_key(&queue[b])))
    });
    if policy.strategy == Strategy::PriorityFifo {
        return idx;
    }
    let mut out = Vec::with_capacity(idx.len());
    let mut start = 0;
    while start < idx.len() {
        let end = idx[start..]
            .iter()
            .position(|&i| prio[i] != prio[idx[start]])
            .map_or(idx.len(), |p| start + p);
        out.extend(interleave(policy, queue, &idx[start..end], served, now));
        start = end;
    }
    out
}

struct Group {
    entries: VecDeque<usize>,
    usage: GroupUsage,
    weight: u64,
}

fn interleave(
    policy: &RulePolicy,
    queue: &[QueueEntry],
    run: &[usize],
    served: &ServeLog,
    now: Timestamp,
) -> Vec<usize> {
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for &i in run {
        let key = policy.group_of(&queue[i].client);
        let group = groups.entry(key.clone()).or_insert_with(|| Group {
            entries: VecDeque::new(),
            usage: served.usage(&key),
            weight: u64::from(policy.weight_of(&queue[i])),
        });
        group.entries.push_back(i);
    }
    let weighted = policy.strategy == Strategy::Weighted;
    let mut out = Vec::with_capacity(run.len());
    let mut tick = 0u64;
    loop {
        let pick = groups
            .iter()
            .filter(|(_, g)| !g.entries.is_empty())
            .min_by(|(_, a), (_, b)| {
                let share = if weighted {
                    (u128::from(a.usage.granted) * u128::from(b.weight))
                        .cmp(&(u128::from(b.usage.granted) * u128::from(a.weight)))
                } else {
                    Ordering::Equal
                };
                share
                    .then_with(|| a.usage.last_granted.cmp(&b.usage.last_granted))
                    .then_with(|| {
                        fifo_key(&queue[a.entries[0]]).cmp(&fifo_key(&queue[b.entries[0]]))
                    })
            })
            .map(|(k, _)| k.clone());
        let Some(key) = pick else {
            break;
        };
        let group = groups.get_mut(&key).expect("picked group exists");
        if let Some(i) = group.entries.pop_front() {
            out.push(i);
        }
        tick += 1;
        group.usage.last_granted = Some(Timestamp(now.0.saturating_add(tick)));
        group.usage.granted += 1;
    }
    out
}
