use crate::admission::Admission;
use crate::condition::Condition;
use crate::model::{Timestamp, Zone};
use crate::queue::{ClientView, Order, QueueEntry, ServeLog};
use crate::scheduler::{PoolLimits, Reject, Scheduler, Wait};
use crate::strategy::dispatch_order;
use std::str::FromStr;

/// Built-in ordering strategy of `[queue] strategy` (spec 3.5); the list is closed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Strategy {
    /// `priority-fifo`: by descending priority, inside a priority by enqueue time.
    #[default]
    PriorityFifo,
    /// `fair-share`: inside a priority groups take turns, the one longest without resources first.
    FairShare,
    /// `weighted`: like `fair-share`, the share of a group proportional to its `weight`.
    Weighted,
}

impl FromStr for Strategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "priority-fifo" => Ok(Strategy::PriorityFifo),
            "fair-share" => Ok(Strategy::FairShare),
            "weighted" => Ok(Strategy::Weighted),
            other => Err(format!(
                "unknown strategy `{other}`, expected priority-fifo, fair-share or weighted"
            )),
        }
    }
}

/// `[clients] group_by`: which trusted attribute forms a group.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GroupBy {
    /// By client key.
    Key,
    /// By client label.
    #[default]
    Label,
}

impl FromStr for GroupBy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "key" => Ok(GroupBy::Key),
            "label" => Ok(GroupBy::Label),
            other => Err(format!("unknown group_by `{other}`, expected key or label")),
        }
    }
}

/// `reserve = { cpu, mem_mb }`: a share of the pool only matching orders may use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reserve {
    /// Reserved CPU.
    pub cpu: u32,
    /// Reserved MB.
    pub mem_mb: u64,
}

/// `quota = { max_running, max_mem_mb }`: a ceiling for a group.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Quota {
    /// Jobs of the group running at once.
    pub max_running: Option<u32>,
    /// MB of the group running at once.
    pub max_mem_mb: Option<u64>,
}

/// One `[[rule]]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rule {
    /// Optional name; messages use the 1-based number without it.
    pub name: Option<String>,
    /// Condition.
    pub when: Condition,
    /// Priority, greater first.
    pub priority: Option<i64>,
    /// Reserve.
    pub reserve: Option<Reserve>,
    /// Quota of the group.
    pub quota: Option<Quota>,
    /// Weight of the group for `weighted`.
    pub weight: Option<u32>,
}

/// The decisions the broker's planner asks the owner's policy (spec 3.5).
pub trait Policy {
    /// Indices of `queue` in dispatch order at `now`, aging included.
    fn order(&self, queue: &[QueueEntry], served: &ServeLog, now: Timestamp) -> Vec<usize>;
    /// Admission at intake: `Err` when the order can never run under this policy.
    fn admit(&self, order: &Order, client: &ClientView) -> Result<(), Reject>;
    /// Reserve and quota check for starting `entry` while `running` run.
    fn gate(&self, entry: &QueueEntry, running: &[&QueueEntry]) -> Result<(), Wait>;
    /// `[pool]` budget.
    fn pool(&self) -> &PoolLimits;
    /// `[routing] runner_zone_order`: preference between suitable runners.
    fn runner_zone_order(&self) -> &[Zone];
    /// Group key of a client by `[clients] group_by`.
    fn group_of(&self, client: &ClientView) -> String;
    /// `[admission]`: how the real load of a runner machine gates a start.
    fn admission(&self) -> Admission {
        Admission::default()
    }
}

/// The policy of declarative rules and built-in strategies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulePolicy {
    /// `[queue] strategy`.
    pub strategy: Strategy,
    /// `[queue] age_after_min`.
    pub age_after_min: u64,
    /// `[[rule]]` in file order.
    pub rules: Vec<Rule>,
    /// `[clients] group_by`.
    pub group_by: GroupBy,
    /// `[pool]`.
    pub pool: PoolLimits,
    /// `[limits] max_job_timeout_s`; `None` means no ceiling.
    pub max_job_timeout_s: Option<u64>,
    /// `[routing] runner_zone_order`.
    pub runner_zone_order: Vec<Zone>,
    /// `[admission]`.
    pub admission: Admission,
}

/// Default `[queue] age_after_min`.
pub const DEFAULT_AGE_AFTER_MIN: u64 = 30;

impl RulePolicy {
    /// A policy over `pool` without rules: `priority-fifo`, aging after 30 minutes, groups by
    /// label, zone order local, lan, remote. Default rules are data of the policy file loader.
    pub fn with_pool(pool: PoolLimits) -> RulePolicy {
        RulePolicy {
            strategy: Strategy::PriorityFifo,
            age_after_min: DEFAULT_AGE_AFTER_MIN,
            rules: Vec::new(),
            group_by: GroupBy::Label,
            pool,
            max_job_timeout_s: None,
            runner_zone_order: vec![Zone::Local, Zone::Lan, Zone::Remote],
            admission: Admission::default(),
        }
    }

    /// Name of rule `index` for messages: its name, else its 1-based number.
    pub fn rule_name(&self, index: usize) -> String {
        self.rules
            .get(index)
            .and_then(|r| r.name.clone())
            .unwrap_or_else(|| (index + 1).to_string())
    }

    fn first<T>(
        &self,
        order: &Order,
        client: &ClientView,
        field: impl Fn(&Rule) -> Option<T>,
    ) -> Option<(usize, T)> {
        self.rules.iter().enumerate().find_map(|(i, r)| {
            field(r)
                .filter(|_| r.when.matches(order, client))
                .map(|v| (i, v))
        })
    }

    /// Own priority of `entry`: the first matching rule that sets `priority`, else 0.
    pub fn priority_of(&self, entry: &QueueEntry) -> i64 {
        self.first(&entry.order, &entry.client, |r| r.priority)
            .map_or(0, |(_, p)| p)
    }

    /// The highest priority any rule defines, at least 0.
    pub fn max_priority(&self) -> i64 {
        self.rules
            .iter()
            .filter_map(|r| r.priority)
            .fold(0, i64::max)
    }

    /// Priority `entry` is ordered with at `now`: once it has waited longer than
    /// `age_after_min` since the hub enqueued it, the highest defined priority.
    pub fn effective_priority(&self, entry: &QueueEntry, now: Timestamp) -> i64 {
        let own = self.priority_of(entry);
        let waited = now.millis_since(entry.order.enqueued_at);
        if waited > self.age_after_min.saturating_mul(60_000) {
            own.max(self.max_priority())
        } else {
            own
        }
    }

    /// Weight of the group of `entry`: the first matching rule that sets `weight`, else 1.
    pub fn weight_of(&self, entry: &QueueEntry) -> u32 {
        self.first(&entry.order, &entry.client, |r| r.weight)
            .map_or(1, |(_, w)| w.max(1))
    }

    fn reserve_rule(&self, order: &Order, client: &ClientView) -> Option<usize> {
        self.first(order, client, |r| r.reserve).map(|(i, _)| i)
    }

    fn reserves_against(&self, order: &Order, client: &ClientView) -> Vec<(usize, Reserve)> {
        let own = self.reserve_rule(order, client);
        self.rules
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != own)
            .filter_map(|(i, r)| r.reserve.map(|res| (i, res)))
            .collect()
    }
}

impl Policy for RulePolicy {
    fn order(&self, queue: &[QueueEntry], served: &ServeLog, now: Timestamp) -> Vec<usize> {
        dispatch_order(self, queue, served, now)
    }

    fn admit(&self, order: &Order, client: &ClientView) -> Result<(), Reject> {
        let req = &order.requirements;
        if let Some(ceiling) = self.max_job_timeout_s {
            if req.timeout_s > ceiling {
                return Err(Reject::Timeout {
                    needed: req.timeout_s,
                    ceiling,
                });
            }
        }
        Scheduler::new(self.pool.clone()).can_ever_fit(req)?;
        let against = self.reserves_against(order, client);
        let cpu = against
            .iter()
            .fold(0u32, |a, (_, r)| a.saturating_add(r.cpu));
        let mem = against
            .iter()
            .fold(0u64, |a, (_, r)| a.saturating_add(r.mem_mb));
        if req.cpu > self.pool.cpu.saturating_sub(cpu)
            || req.mem_mb > self.pool.mem_mb.saturating_sub(mem)
        {
            if let Some((i, _)) = against.first() {
                return Err(Reject::Reserved {
                    rule: self.rule_name(*i),
                    mem_mb: req.mem_mb,
                    cpu: req.cpu,
                });
            }
        }
        if let Some((i, q)) = self.first(order, client, |r| r.quota) {
            if q.max_running == Some(0) || q.max_mem_mb.is_some_and(|m| req.mem_mb > m) {
                return Err(Reject::Quota {
                    rule: self.rule_name(i),
                    mem_mb: req.mem_mb,
                    max_mem_mb: q.max_mem_mb,
                    max_running: q.max_running,
                });
            }
        }
        Ok(())
    }

    fn gate(&self, entry: &QueueEntry, running: &[&QueueEntry]) -> Result<(), Wait> {
        let req = &entry.order.requirements;
        let used_cpu = running
            .iter()
            .fold(0u32, |a, e| a.saturating_add(e.order.requirements.cpu));
        let used_mem = running
            .iter()
            .fold(0u64, |a, e| a.saturating_add(e.order.requirements.mem_mb));
        let mut blocked_cpu = 0u32;
        let mut blocked_mem = 0u64;
        let mut first_blocking = None;
        for (i, res) in self.reserves_against(&entry.order, &entry.client) {
            let mine = running
                .iter()
                .filter(|e| self.reserve_rule(&e.order, &e.client) == Some(i));
            let (cpu, mem) = mine.fold((0u32, 0u64), |(c, m), e| {
                (
                    c.saturating_add(e.order.requirements.cpu),
                    m.saturating_add(e.order.requirements.mem_mb),
                )
            });
            let left_cpu = res.cpu.saturating_sub(cpu);
            let left_mem = res.mem_mb.saturating_sub(mem);
            if (left_cpu > 0 || left_mem > 0) && first_blocking.is_none() {
                first_blocking = Some(i);
            }
            blocked_cpu = blocked_cpu.saturating_add(left_cpu);
            blocked_mem = blocked_mem.saturating_add(left_mem);
        }
        let free_cpu = self
            .pool
            .cpu
            .saturating_sub(used_cpu.saturating_add(blocked_cpu));
        let free_mem = self
            .pool
            .mem_mb
            .saturating_sub(used_mem.saturating_add(blocked_mem));
        if let Some(i) = first_blocking {
            if req.cpu > free_cpu || req.mem_mb > free_mem {
                return Err(Wait::Reserved {
                    rule: self.rule_name(i),
                    cpu: blocked_cpu,
                    pool_cpu: self.pool.cpu,
                    mem_mb: blocked_mem,
                    pool_mem_mb: self.pool.mem_mb,
                });
            }
        }
        if let Some((i, q)) = self.first(&entry.order, &entry.client, |r| r.quota) {
            let group = self.group_of(&entry.client);
            let mine: Vec<&&QueueEntry> = running
                .iter()
                .filter(|e| self.group_of(&e.client) == group)
                .collect();
            let count = mine.len() as u32;
            let mem = mine
                .iter()
                .fold(0u64, |a, e| a.saturating_add(e.order.requirements.mem_mb));
            let over_count = q.max_running.is_some_and(|m| count >= m);
            let over_mem = q
                .max_mem_mb
                .is_some_and(|m| mem.saturating_add(req.mem_mb) > m);
            if over_count || over_mem {
                return Err(Wait::Quota {
                    rule: self.rule_name(i),
                    running: count,
                    max_running: q.max_running,
                    mem_mb: mem,
                    max_mem_mb: q.max_mem_mb,
                });
            }
        }
        Ok(())
    }

    fn pool(&self) -> &PoolLimits {
        &self.pool
    }

    fn runner_zone_order(&self) -> &[Zone] {
        &self.runner_zone_order
    }

    fn group_of(&self, client: &ClientView) -> String {
        match self.group_by {
            GroupBy::Key => client.key.clone(),
            GroupBy::Label => client.label.clone(),
        }
    }

    fn admission(&self) -> Admission {
        self.admission
    }
}
