use crate::admission::LoadResource;
use crate::matching::Mismatch;
use crate::model::{JobId, Timestamp};
use crate::requirements::Requirements;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// `[pool]` of the broker policy: the shared budget of one pool (spec 3.6).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolLimits {
    /// Memory of the pool in MB.
    pub mem_mb: u64,
    /// CPU of the pool.
    pub cpu: u32,
    /// Tags that one job at a time may hold, whether or not the order marks them exclusive.
    pub exclusive_tags: BTreeSet<String>,
}

/// Free part of the pool budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Free {
    /// Free memory in MB.
    pub mem_mb: u64,
    /// Free CPU.
    pub cpu: u32,
}

/// Why an order cannot start right now; it stays queued and shows the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wait {
    /// Not enough free pool memory.
    Memory {
        /// Needed MB.
        needed: u64,
        /// Free MB.
        free: u64,
    },
    /// Not enough free pool CPU.
    Cpu {
        /// Needed CPU.
        needed: u32,
        /// Free CPU.
        free: u32,
    },
    /// A tag is held by another job and one of the two holds it exclusively.
    Exclusive {
        /// The contested tag.
        tag: String,
        /// The job holding it.
        holder: JobId,
    },
    /// Part of the pool is reserved by a rule this order does not match.
    Reserved {
        /// Rule name or number.
        rule: String,
        /// Reserved CPU still unused by its orders.
        cpu: u32,
        /// CPU of the pool.
        pool_cpu: u32,
        /// Reserved MB still unused by its orders.
        mem_mb: u64,
        /// MB of the pool.
        pool_mem_mb: u64,
    },
    /// The group of the order reached its quota.
    Quota {
        /// Rule name or number.
        rule: String,
        /// Jobs of the group running now.
        running: u32,
        /// `max_running` of the quota, if set.
        max_running: Option<u32>,
        /// MB of the group running now.
        mem_mb: u64,
        /// `max_mem_mb` of the quota, if set.
        max_mem_mb: Option<u64>,
    },
    /// Every runner that suits the order is busy.
    RunnerBusy,
    /// Every runner that suits the order is offline.
    RunnerOffline,
    /// No registered runner suits the order.
    NoRunner,
    /// Starting now would delay the reserved start of the head order.
    HeadReserved {
        /// The head order.
        head: JobId,
        /// Its reserved start.
        at: Timestamp,
    },
    /// The machine of every suitable runner is busy with processes outside localrunner.
    ForeignLoad {
        /// The resource that is short.
        resource: LoadResource,
        /// Needed MB (with the headroom) or CPU.
        needed: u64,
        /// Free MB, or foreign CPU busy percent.
        level: u64,
    },
}

impl fmt::Display for Wait {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory { needed, free } => write!(f, "нужно {needed} МБ, свободно {free}"),
            Self::Cpu { needed, free } => write!(f, "нужно {needed} CPU, свободно {free}"),
            Self::Exclusive { tag, holder } => write!(f, "тег {tag} занят джобой {holder}"),
            Self::Reserved {
                rule,
                cpu,
                pool_cpu,
                mem_mb,
                pool_mem_mb,
            } => {
                let mut parts = Vec::new();
                if *cpu > 0 {
                    parts.push(format!("{cpu} из {pool_cpu} ядер"));
                }
                if *mem_mb > 0 {
                    parts.push(format!("{mem_mb} из {pool_mem_mb} МБ"));
                }
                write!(f, "правило {rule}: {} зарезервированы", parts.join(" и "))
            }
            Self::Quota {
                rule,
                running,
                max_running,
                mem_mb,
                max_mem_mb,
            } => {
                write!(f, "правило {rule}: квота группы, запущено {running}")?;
                if let Some(max) = max_running {
                    write!(f, " из {max}")?;
                }
                write!(f, ", память {mem_mb} МБ")?;
                if let Some(max) = max_mem_mb {
                    write!(f, " из {max}")?;
                }
                Ok(())
            }
            Self::RunnerBusy => write!(f, "раннер занят"),
            Self::RunnerOffline => write!(f, "подходящие раннеры offline"),
            Self::NoRunner => write!(f, "нет подходящего раннера"),
            Self::HeadReserved { head, at } => {
                write!(f, "старт зарезервирован для {head} на {at}")
            }
            Self::ForeignLoad {
                resource: LoadResource::Memory,
                needed,
                level,
            } => write!(
                f,
                "занято чужими процессами: свободно {level} МБ, нужно {needed}"
            ),
            Self::ForeignLoad {
                resource: LoadResource::Cpu,
                needed,
                level,
            } => write!(
                f,
                "занято чужими процессами: CPU {level}%, нужно {needed} ядер"
            ),
        }
    }
}

/// Why an order is refused at intake: it can never run here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    /// The order needs more memory than the whole pool.
    PoolMemory {
        /// Needed MB.
        needed: u64,
        /// Pool MB.
        limit: u64,
    },
    /// The order needs more CPU than the whole pool.
    PoolCpu {
        /// Needed CPU.
        needed: u32,
        /// Pool CPU.
        limit: u32,
    },
    /// The order timeout exceeds `[limits] max_job_timeout_s`.
    Timeout {
        /// Needed seconds.
        needed: u64,
        /// Ceiling seconds.
        ceiling: u64,
    },
    /// Reserves of rules the order does not match leave it too little of the pool.
    Reserved {
        /// Rule name or number.
        rule: String,
        /// Needed MB.
        mem_mb: u64,
        /// Needed CPU.
        cpu: u32,
    },
    /// The quota of the group is below the order even with nothing running.
    Quota {
        /// Rule name or number.
        rule: String,
        /// Needed MB.
        mem_mb: u64,
        /// `max_mem_mb` of the quota.
        max_mem_mb: Option<u64>,
        /// `max_running` of the quota.
        max_running: Option<u32>,
    },
    /// No registered runner suits the order; every runner with its reasons.
    NoMatchingRunner {
        /// Runner name and why it does not suit.
        runners: Vec<(String, Mismatch)>,
    },
    /// A same-machine order and no runner of that installation is online.
    NoRunnerOnMachine,
}

impl fmt::Display for Reject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PoolMemory { needed, limit } => {
                write!(f, "нужно {needed} МБ, максимум в пуле {limit}")
            }
            Self::PoolCpu { needed, limit } => {
                write!(f, "нужно {needed} CPU, максимум в пуле {limit}")
            }
            Self::Timeout { needed, ceiling } => {
                write!(f, "потолок таймаута {ceiling} с, нужно {needed}")
            }
            Self::Reserved { rule, mem_mb, cpu } => write!(
                f,
                "правило {rule}: после резерва в пуле не остаётся {mem_mb} МБ и {cpu} CPU"
            ),
            Self::Quota {
                rule,
                mem_mb,
                max_mem_mb,
                max_running,
            } => {
                write!(f, "правило {rule}: квота группы")?;
                if let Some(max) = max_mem_mb {
                    write!(f, " {max} МБ меньше {mem_mb}")?;
                }
                if let Some(max) = max_running {
                    write!(f, " max_running {max}")?;
                }
                Ok(())
            }
            Self::NoMatchingRunner { runners } => {
                if runners.is_empty() {
                    return write!(f, "нет зарегистрированных раннеров");
                }
                let parts: Vec<String> = runners.iter().map(|(n, m)| format!("{n}: {m}")).collect();
                f.write_str(&parts.join(" | "))
            }
            Self::NoRunnerOnMachine => {
                write!(f, "нет раннера на вашей установке, запустите его: up")
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Grant {
    mem_mb: u64,
    cpu: u32,
    tags: BTreeSet<String>,
    exclusive: BTreeSet<String>,
}

/// Pure bookkeeping of the pool budget: memory, CPU and held tags (spec 3.6, 6).
#[derive(Debug, Clone)]
pub struct Scheduler {
    limits: PoolLimits,
    grants: BTreeMap<JobId, Grant>,
}

impl Scheduler {
    /// An empty pool with these limits.
    pub fn new(limits: PoolLimits) -> Scheduler {
        Scheduler {
            limits,
            grants: BTreeMap::new(),
        }
    }

    /// Limits of the pool.
    pub fn limits(&self) -> &PoolLimits {
        &self.limits
    }

    /// Checks that `req` fits an otherwise empty pool.
    pub fn can_ever_fit(&self, req: &Requirements) -> Result<(), Reject> {
        if req.mem_mb > self.limits.mem_mb {
            return Err(Reject::PoolMemory {
                needed: req.mem_mb,
                limit: self.limits.mem_mb,
            });
        }
        if req.cpu > self.limits.cpu {
            return Err(Reject::PoolCpu {
                needed: req.cpu,
                limit: self.limits.cpu,
            });
        }
        Ok(())
    }

    /// Free memory and CPU.
    pub fn free(&self) -> Free {
        let mem = self
            .grants
            .values()
            .fold(0u64, |a, g| a.saturating_add(g.mem_mb));
        let cpu = self
            .grants
            .values()
            .fold(0u32, |a, g| a.saturating_add(g.cpu));
        Free {
            mem_mb: self.limits.mem_mb.saturating_sub(mem),
            cpu: self.limits.cpu.saturating_sub(cpu),
        }
    }

    /// Whether `job` holds a grant.
    pub fn holds(&self, job: &JobId) -> bool {
        self.grants.contains_key(job)
    }

    fn grant_of(&self, req: &Requirements) -> Grant {
        let mut tags = req.tags.clone();
        tags.extend(req.exclusive.iter().cloned());
        let mut exclusive = req.exclusive.clone();
        exclusive.extend(self.limits.exclusive_tags.intersection(&tags).cloned());
        Grant {
            mem_mb: req.mem_mb,
            cpu: req.cpu,
            tags,
            exclusive,
        }
    }

    /// Checks without side effects whether [`Scheduler::try_grant`] would succeed now.
    pub fn can_grant(&self, job: &JobId, req: &Requirements) -> Result<(), Wait> {
        if self.holds(job) {
            return Ok(());
        }
        let free = self.free();
        if req.mem_mb > free.mem_mb {
            return Err(Wait::Memory {
                needed: req.mem_mb,
                free: free.mem_mb,
            });
        }
        if req.cpu > free.cpu {
            return Err(Wait::Cpu {
                needed: req.cpu,
                free: free.cpu,
            });
        }
        let want = self.grant_of(req);
        for tag in &want.tags {
            for (holder, held) in &self.grants {
                if held.tags.contains(tag)
                    && (want.exclusive.contains(tag) || held.exclusive.contains(tag))
                {
                    return Err(Wait::Exclusive {
                        tag: tag.clone(),
                        holder: holder.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Grants memory, CPU and tags of `req` all at once or nothing; a job that already holds
    /// a grant keeps it unchanged.
    pub fn try_grant(&mut self, job: &JobId, req: &Requirements) -> Result<(), Wait> {
        self.can_grant(job, req)?;
        if !self.holds(job) {
            let grant = self.grant_of(req);
            self.grants.insert(job.clone(), grant);
        }
        Ok(())
    }

    /// Records the grant of a job that already runs, without checks (for example after a
    /// broker restart or a policy reload that shrank the pool).
    pub fn insert_running(&mut self, job: &JobId, req: &Requirements) {
        let grant = self.grant_of(req);
        self.grants.insert(job.clone(), grant);
    }

    /// Releases the grant of `job`; an unknown job is ignored.
    pub fn release(&mut self, job: &JobId) {
        self.grants.remove(job);
    }
}
