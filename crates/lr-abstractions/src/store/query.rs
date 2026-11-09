use super::broker_rows::{JobRecord, JobRow, SourceKind};
use lr_core::{JobState, StepState, Timestamp};
use std::collections::BTreeMap;

/// Filter of `history` (spec 8); every set field must match, `limit` caps the result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobFilter {
    /// Client key.
    pub client: Option<String>,
    /// Project.
    pub project: Option<String>,
    /// Source kind.
    pub source: Option<SourceKind>,
    /// Label `--as`.
    pub label: Option<String>,
    /// Profile.
    pub profile: Option<String>,
    /// State.
    pub state: Option<JobState>,
    /// Platform.
    pub platform: Option<String>,
    /// Runner.
    pub runner: Option<String>,
    /// Only runs queued at or after this time.
    pub since: Option<Timestamp>,
    /// Only runs that are not final.
    pub active_only: bool,
    /// Maximum number of runs.
    pub limit: Option<usize>,
}

impl JobFilter {
    /// Whether `job` passes every set field except `limit`.
    pub fn matches(&self, job: &JobRow) -> bool {
        fn eq<T: PartialEq>(want: &Option<T>, have: &T) -> bool {
            want.as_ref().is_none_or(|w| w == have)
        }
        eq(&self.client, &job.key.client)
            && eq(&self.project, &job.project)
            && eq(&self.source, &job.source)
            && eq(&self.label, &job.label)
            && eq(&self.profile, &job.profile)
            && eq(&self.state, &job.state)
            && self
                .platform
                .as_ref()
                .is_none_or(|p| job.platform.as_ref() == Some(p))
            && self
                .runner
                .as_ref()
                .is_none_or(|r| job.runner.as_ref() == Some(r))
            && self.since.is_none_or(|s| job.queued_at >= s)
            && (!self.active_only || !job.state.is_final())
    }
}

/// Filter of `stats` (spec 8).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatsQuery {
    /// Client key; the owner of the broker leaves it empty to see the whole pool.
    pub client: Option<String>,
    /// Project.
    pub project: Option<String>,
    /// Profile.
    pub profile: Option<String>,
    /// Only runs queued at or after this time.
    pub since: Option<Timestamp>,
}

/// Median and 95th percentile, nearest rank; `None` without samples.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Spread {
    /// 50th percentile.
    pub median: Option<u64>,
    /// 95th percentile.
    pub p95: Option<u64>,
}

impl Spread {
    fn of(mut samples: Vec<u64>) -> Spread {
        Spread {
            median: nearest_rank(&mut samples, 50),
            p95: nearest_rank(&mut samples, 95),
        }
    }
}

/// Statistics of one step name within a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepStats {
    /// Step name.
    pub name: String,
    /// Steps that started.
    pub runs: u64,
    /// Steps that passed.
    pub passed: u64,
    /// Duration of started and ended steps, ms.
    pub duration_ms: Spread,
}

/// Statistics of one profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileStats {
    /// Profile name.
    pub profile: String,
    /// Final runs.
    pub runs: u64,
    /// Passed runs.
    pub passed: u64,
    /// Failed runs.
    pub failed: u64,
    /// Timed out runs.
    pub timeouts: u64,
    /// Cancelled runs.
    pub cancelled: u64,
    /// Lost runs.
    pub lost: u64,
    /// Wait from `queued_at` to `started_at`, ms, for runs that started.
    pub wait_ms: Spread,
    /// Duration from `started_at` to `finished_at`, ms, for runs that started.
    pub duration_ms: Spread,
    /// Per step name, in step order of first appearance.
    pub steps: Vec<StepStats>,
}

/// How much of the pool a client used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientUsage {
    /// Client key.
    pub client: String,
    /// Client label.
    pub label: String,
    /// Final runs.
    pub runs: u64,
    /// Sum of run durations, ms.
    pub busy_ms: u64,
}

/// Statistics of the history (spec 8 `stats`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    /// Per profile, by name.
    pub profiles: Vec<ProfileStats>,
    /// Per client, by client key.
    pub clients: Vec<ClientUsage>,
}

/// Nearest-rank percentile `pct` (1..=100) of `values`; sorts them. `None` when empty.
pub fn nearest_rank(values: &mut [u64], pct: u8) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let n = values.len();
    let rank = (usize::from(pct.clamp(1, 100)) * n).div_ceil(100);
    Some(values[rank.max(1) - 1])
}

fn span(from: Option<Timestamp>, to: Option<Timestamp>) -> Option<u64> {
    Some(to?.millis_since(from?))
}

/// The statistics every [`super::BrokerStore`] returns: computed over final runs matching
/// `query`, so the SQLite store and the fake agree by construction.
pub fn compute_stats(jobs: &[JobRecord], query: &StatsQuery) -> Stats {
    let filter = JobFilter {
        client: query.client.clone(),
        project: query.project.clone(),
        profile: query.profile.clone(),
        since: query.since,
        ..JobFilter::default()
    };
    let selected: Vec<&JobRecord> = jobs
        .iter()
        .filter(|r| r.job.state.is_final() && filter.matches(&r.job))
        .collect();
    let mut by_profile: BTreeMap<&str, Vec<&JobRecord>> = BTreeMap::new();
    let mut clients: BTreeMap<&str, ClientUsage> = BTreeMap::new();
    for r in &selected {
        by_profile.entry(&r.job.profile).or_default().push(r);
        let usage = clients
            .entry(&r.job.key.client)
            .or_insert_with(|| ClientUsage {
                client: r.job.key.client.clone(),
                label: r.job.client_label.clone(),
                runs: 0,
                busy_ms: 0,
            });
        usage.runs += 1;
        usage.busy_ms += span(r.job.started_at, r.job.finished_at).unwrap_or(0);
    }
    Stats {
        profiles: by_profile
            .into_iter()
            .map(|(name, runs)| profile_stats(name, &runs))
            .collect(),
        clients: clients.into_values().collect(),
    }
}

fn profile_stats(name: &str, runs: &[&JobRecord]) -> ProfileStats {
    let count = |s: JobState| runs.iter().filter(|r| r.job.state == s).count() as u64;
    let started: Vec<&&JobRecord> = runs.iter().filter(|r| r.job.started_at.is_some()).collect();
    let mut steps: Vec<(String, Vec<u64>, u64, u64)> = Vec::new();
    for r in runs {
        for s in &r.steps {
            if s.started_at.is_none() {
                continue;
            }
            let i = match steps.iter().position(|e| e.0 == s.name) {
                Some(i) => i,
                None => {
                    steps.push((s.name.clone(), Vec::new(), 0, 0));
                    steps.len() - 1
                }
            };
            let entry = &mut steps[i];
            entry.2 += 1;
            if s.state == StepState::Passed {
                entry.3 += 1;
            }
            if let Some(d) = span(s.started_at, s.finished_at) {
                entry.1.push(d);
            }
        }
    }
    ProfileStats {
        profile: name.to_string(),
        runs: runs.len() as u64,
        passed: count(JobState::Passed),
        failed: count(JobState::Failed),
        timeouts: count(JobState::Timeout),
        cancelled: count(JobState::Cancelled),
        lost: count(JobState::Lost),
        wait_ms: Spread::of(
            started
                .iter()
                .filter_map(|r| span(Some(r.job.queued_at), r.job.started_at))
                .collect(),
        ),
        duration_ms: Spread::of(
            started
                .iter()
                .filter_map(|r| span(r.job.started_at, r.job.finished_at))
                .collect(),
        ),
        steps: steps
            .into_iter()
            .map(|(name, durations, runs, passed)| StepStats {
                name,
                runs,
                passed,
                duration_ms: Spread::of(durations),
            })
            .collect(),
    }
}
