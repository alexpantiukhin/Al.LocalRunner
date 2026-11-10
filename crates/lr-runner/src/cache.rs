use crate::diag::Diag;
use crate::settings::Retention;
use crate::source::dir_name;
use lr_abstractions::{CacheEntry, CacheKey, Files, Level, RunnerStore, StoreError};
use lr_core::{cache_env_var, Timestamp};
use std::path::{Path, PathBuf};

const DAY_MS: u64 = 86_400_000;
const MB: u64 = 1024 * 1024;

/// Client part of the index key of workspace mirrors, which belong to no client; a cache
/// key always names its client.
pub const WORKSPACE_CLIENT: &str = "";

/// Where named caches live: `<state>/cache/<client>/<project>/<name>`, each part a safe
/// directory name made unique by a hash, so two clients never share a directory unless the
/// broker sends them the same client key (`[cache] share = "project"`).
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

/// Entries to remove (spec 3.10): first those unused for longer than `retention_days`, then,
/// while the rest is above `max_size_gb`, the least recently used (`lru`). Entries in `in_use`
/// are never removed.
pub fn plan_sweep(
    entries: &[CacheEntry],
    now: Timestamp,
    limits: &Retention,
    in_use: &[CacheKey],
) -> Vec<CacheKey> {
    let mut doomed = Vec::new();
    let mut kept: Vec<&CacheEntry> = Vec::new();
    for e in entries {
        let stale = limits
            .retention_days
            .is_some_and(|d| now.millis_since(e.last_used) > d.saturating_mul(DAY_MS));
        if stale && !in_use.contains(&e.key) {
            doomed.push(e.key.clone());
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
                doomed.push(e.key.clone());
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
    pub(crate) layout: CacheLayout,
    pub(crate) cache: Retention,
    pub(crate) workspace: Retention,
}

impl Caches<'_> {
    fn store_err(&self, what: &str, e: &StoreError) {
        self.diag
            .event(Level::Error, what, &[("error", &e.to_string())]);
    }

    pub(crate) async fn sweep(&self, now: Timestamp, in_use: &[CacheKey]) {
        let entries = match self.store.cache_entries().await {
            Ok(e) => e,
            Err(e) => return self.store_err("cache index unreadable", &e),
        };
        let (ws, caches): (Vec<CacheEntry>, Vec<CacheEntry>) =
            entries.into_iter().partition(is_workspace);
        let mut doomed = plan_sweep(&caches, now, &self.cache, in_use);
        doomed.extend(plan_sweep(&ws, now, &self.workspace, in_use));
        for key in doomed {
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
            if let Err(err) = self.store.remove_cache(&key).await {
                self.store_err("cache entry not removed", &err);
            }
            self.diag
                .event(Level::Info, "cache entry removed", &[("path", &e.path)]);
        }
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
            Ok(entries) => Some(
                entries
                    .iter()
                    .filter(|e| !is_workspace(e))
                    .fold(0u64, |t, e| t.saturating_add(e.bytes))
                    / MB,
            ),
            Err(e) => {
                self.store_err("cache index unreadable", &e);
                None
            }
        }
    }
}
