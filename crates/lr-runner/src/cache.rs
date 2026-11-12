use crate::diag::Diag;
use crate::settings::Retention;
use crate::source::dir_name;
use lr_abstractions::metric::{CACHE_EVICTED_TOTAL, CACHE_USED_MB, REASON, RUNNER};
use lr_abstractions::{CacheEntry, CacheKey, Files, Level, Meter, RunnerStore, StoreError};
use lr_core::{cache_env_var, Timestamp};
use std::path::{Path, PathBuf};

const DAY_MS: u64 = 86_400_000;
const MB: u64 = 1024 * 1024;

/// Client part of the index key of workspace mirrors, which belong to no client; a cache
/// key always names its client.
pub const WORKSPACE_CLIENT: &str = "";

/// Where named caches live: `<state>/cache/<client>/<project>/<name>`, each part a safe
/// directory name made unique by a hash, so two clients never share a directory unless the
/// broker sends them the same cache client (`[cache] share = "project"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheLayout {
    root: PathBuf,
}

impl CacheLayout {
    /// The layout inside the runner's state directory.
    pub fn new(state_dir: PathBuf) -> CacheLayout {
        CacheLayout {
            root: state_dir.join("cache"),
        }
    }

    /// Directory of the cache `key`.
    pub fn path(&self, key: &CacheKey) -> PathBuf {
        self.root
            .join(dir_name("c", &[&key.client]))
            .join(dir_name(&key.project, &[&key.project]))
            .join(dir_name(&key.name, &[&key.name]))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Eviction {
    Age,
    Size,
}

impl Eviction {
    fn label(self) -> &'static str {
        match self {
            Eviction::Age => "age",
            Eviction::Size => "size",
        }
    }
}

/// Entries to remove (spec 3.10): first those unused for longer than `retention_days`, then,
/// while the rest is above `max_size_gb`, the least recently used (`lru`). Entries in `in_use`
/// are never removed.
pub fn plan_sweep(
    entries: &[CacheEntry],
    now: Timestamp,
    limits: &Retention,
    in_use: &[CacheKey],
) -> Vec<CacheKey> {
    plan_evictions(entries, now, limits, in_use)
        .into_iter()
        .map(|(key, _)| key)
        .collect()
}

fn plan_evictions(
    entries: &[CacheEntry],
    now: Timestamp,
    limits: &Retention,
    in_use: &[CacheKey],
) -> Vec<(CacheKey, Eviction)> {
    let mut doomed = Vec::new();
    let mut kept: Vec<&CacheEntry> = Vec::new();
    for e in entries {
        let stale = limits
            .retention_days
            .is_some_and(|d| now.millis_since(e.last_used) > d.saturating_mul(DAY_MS));
        if stale && !in_use.contains(&e.key) {
            doomed.push((e.key.clone(), Eviction::Age));
        } else {
            kept.push(e);
        }
    }
    if let Some(max) = limits.max_bytes() {
        let mut total = kept.iter().fold(0u64, |t, e| t.saturating_add(e.bytes));
        kept.sort_by_key(|e| (e.last_used, e.key.clone()));
        for e in kept {
            if total <= max {
                break;
            }
            if !in_use.contains(&e.key) {
                total = total.saturating_sub(e.bytes);
                doomed.push((e.key.clone(), Eviction::Size));
            }
        }
    }
    doomed
}

fn is_workspace(e: &CacheEntry) -> bool {
    e.key.client == WORKSPACE_CLIENT
}

pub(crate) struct Caches<'a> {
    pub(crate) store: &'a dyn RunnerStore,
    pub(crate) files: &'a dyn Files,
    pub(crate) diag: &'a Diag,
    pub(crate) meter: &'a dyn Meter,
    pub(crate) runner: &'a str,
    pub(crate) layout: CacheLayout,
    pub(crate) cache: Retention,
    pub(crate) workspace: Retention,
}

impl Caches<'_> {
    fn store_err(&self, what: &str, e: &StoreError) {
        self.diag
            .event(Level::Error, what, &[("error", &e.to_string())]);
    }

    async fn remove_job_copies(&self, mirror: &str) {
        let Some(jobs) = Path::new(mirror).parent().map(|p| p.join("jobs")) else {
            return;
        };
        if let Err(err) = self.files.remove_dir_all(&jobs).await {
            self.diag.event(
                Level::Error,
                "job checkouts not removed",
                &[
                    ("path", &jobs.display().to_string()),
                    ("error", &err.to_string()),
                ],
            );
        }
    }

    /// Removes the checkouts of every workspace (spec 3.7): called at start, when no job runs,
    /// so the copies a crash or a failed removal left behind do not pile up.
    pub(crate) async fn remove_left_checkouts(&self) {
        let entries = match self.store.cache_entries().await {
            Ok(e) => e,
            Err(e) => return self.store_err("cache index unreadable", &e),
        };
        for e in entries.iter().filter(|e| is_workspace(e)) {
            self.remove_job_copies(&e.path).await;
        }
    }

    pub(crate) async fn sweep(&self, now: Timestamp, in_use: &[CacheKey]) {
        let entries = match self.store.cache_entries().await {
            Ok(e) => e,
            Err(e) => return self.store_err("cache index unreadable", &e),
        };
        let (ws, caches): (Vec<CacheEntry>, Vec<CacheEntry>) =
            entries.into_iter().partition(is_workspace);
        let mut doomed: Vec<(CacheKey, Option<Eviction>)> =
            plan_evictions(&caches, now, &self.cache, in_use)
                .into_iter()
                .map(|(k, why)| (k, Some(why)))
                .collect();
        doomed.extend(
            plan_sweep(&ws, now, &self.workspace, in_use)
                .into_iter()
                .map(|k| (k, None)),
        );
        for (key, why) in doomed {
            let Some(e) = caches.iter().chain(ws.iter()).find(|e| e.key == key) else {
                continue;
            };
            if let Err(err) = self.files.remove_dir_all(Path::new(&e.path)).await {
                self.diag.event(
                    Level::Error,
                    "cache directory not removed",
                    &[("path", &e.path), ("error", &err.to_string())],
                );
                continue;
            }
            if why.is_none() {
                self.remove_job_copies(&e.path).await;
            }
            if let Err(err) = self.store.remove_cache(&key).await {
                self.store_err("cache entry not removed", &err);
            }
            self.diag
                .event(Level::Info, "cache entry removed", &[("path", &e.path)]);
            if let Some(why) = why {
                let labels = [(RUNNER, self.runner), (REASON, why.label())];
                self.meter.add(CACHE_EVICTED_TOTAL, &labels, 1);
            }
        }
        self.used_mb().await;
    }

    pub(crate) async fn touch(&self, key: CacheKey, path: &Path, now: Timestamp) {
        let bytes = match self.files.size(path).await {
            Ok(b) => b,
            Err(e) => {
                self.diag.event(
                    Level::Warn,
                    "cache size unreadable",
                    &[("error", &e.to_string())],
                );
                0
            }
        };
        let entry = CacheEntry {
            key,
            path: path.display().to_string(),
            bytes,
            last_used: now,
        };
        if let Err(e) = self.store.upsert_cache(entry).await {
            self.store_err("cache entry not stored", &e);
        }
    }

    pub(crate) async fn open(
        &self,
        client: &str,
        project: &str,
        names: &[String],
        now: Timestamp,
    ) -> Result<(Vec<CacheKey>, Vec<(String, String)>), String> {
        let mut keys = Vec::new();
        let mut env = Vec::new();
        for name in names {
            let key = CacheKey {
                client: client.to_string(),
                project: project.to_string(),
                name: name.clone(),
            };
            let path = self.layout.path(&key);
            self.files
                .create_dir_all(&path)
                .await
                .map_err(|e| format!("cache {name} not created: {}", e.0))?;
            self.touch(key.clone(), &path, now).await;
            env.push((cache_env_var(name), path.display().to_string()));
            keys.push(key);
        }
        Ok((keys, env))
    }

    pub(crate) async fn used_mb(&self) -> Option<u64> {
        match self.store.cache_entries().await {
            Ok(entries) => {
                let mb = entries
                    .iter()
                    .filter(|e| !is_workspace(e))
                    .fold(0u64, |t, e| t.saturating_add(e.bytes))
                    / MB;
                self.meter
                    .set(CACHE_USED_MB, &[(RUNNER, self.runner)], mb as f64);
                Some(mb)
            }
            Err(e) => {
                self.store_err("cache index unreadable", &e);
                None
            }
        }
    }
}
