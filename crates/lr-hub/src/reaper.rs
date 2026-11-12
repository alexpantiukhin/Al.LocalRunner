//! Dead-submitter rule (spec 7.3): the submitter of a job that is not `--detach` is checked
//! every 5 seconds by pid and start time; a process found dead is checked once more 2 seconds
//! later, and only two dead answers in a row make it dead. An unknown answer (start time not
//! obtainable) never counts as dead: the job is kept and a warning is written once.

use lr_abstractions::{Liveness, ProcessInspector};
use lr_core::{JobId, Timestamp};
use std::collections::{BTreeMap, BTreeSet};

/// Seconds between two liveness rounds.
pub const CHECK_PERIOD_S: u64 = 5;
/// Seconds between a dead answer and its confirmation.
pub const CONFIRM_AFTER_S: u64 = 2;

/// A submitter to watch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watched {
    /// The job.
    pub job: JobId,
    /// Submitter process id.
    pub pid: u32,
    /// Submitter start fingerprint, `None` when it could not be obtained.
    pub start: Option<u64>,
}

/// Result of one [`Reaper::tick`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sweep {
    /// Jobs whose submitter was dead at two checks in a row.
    pub dead: Vec<JobId>,
    /// Jobs whose submitter liveness became unknown (warn once, never reap).
    pub unknown: Vec<JobId>,
}

/// The reaper's schedule and its suspects.
#[derive(Debug, Clone)]
pub struct Reaper {
    next_check: Timestamp,
    suspects: BTreeMap<JobId, Timestamp>,
    warned: BTreeSet<JobId>,
}

fn liveness(inspector: &dyn ProcessInspector, w: &Watched) -> Liveness {
    match w.start {
        Some(start) => inspector.liveness(w.pid, start),
        None => Liveness::Unknown,
    }
}

impl Reaper {
    /// The first round is due at `now` (the hub checks submitters at start).
    pub fn new(now: Timestamp) -> Reaper {
        Reaper {
            next_check: now,
            suspects: BTreeMap::new(),
            warned: BTreeSet::new(),
        }
    }

    /// Runs what is due at `now` over the currently watched submitters.
    pub fn tick(
        &mut self,
        now: Timestamp,
        watched: &[Watched],
        inspector: &dyn ProcessInspector,
    ) -> Sweep {
        let mut sweep = Sweep::default();
        let live: BTreeSet<&JobId> = watched.iter().map(|w| &w.job).collect();
        self.suspects.retain(|j, _| live.contains(j));
        self.warned.retain(|j| live.contains(j));
        for w in watched {
            let Some(at) = self.suspects.get(&w.job).copied() else {
                continue;
            };
            if at > now {
                continue;
            }
            self.suspects.remove(&w.job);
            if liveness(inspector, w) == Liveness::Dead {
                sweep.dead.push(w.job.clone());
            }
        }
        if now < self.next_check {
            return sweep;
        }
        self.next_check = now.plus_secs(CHECK_PERIOD_S);
        for w in watched {
            if self.suspects.contains_key(&w.job) || sweep.dead.contains(&w.job) {
                continue;
            }
            match liveness(inspector, w) {
                Liveness::Dead => {
                    self.suspects
                        .insert(w.job.clone(), now.plus_secs(CONFIRM_AFTER_S));
                }
                Liveness::Unknown => {
                    if self.warned.insert(w.job.clone()) {
                        sweep.unknown.push(w.job.clone());
                    }
                }
                Liveness::Alive => {}
            }
        }
        sweep
    }
}
