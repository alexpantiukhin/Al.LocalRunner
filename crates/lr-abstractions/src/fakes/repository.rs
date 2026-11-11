use crate::repository::{Repository, RepositoryError};
use async_trait::async_trait;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    heads: BTreeMap<PathBuf, String>,
    origins: BTreeMap<PathBuf, String>,
    pushed: BTreeSet<String>,
    uncommitted: BTreeSet<PathBuf>,
    files: BTreeMap<(String, String), Vec<u8>>,
    failing: Option<String>,
}

/// Repositories in memory: `HEAD` and `origin` per work tree root, pushed commits and
/// committed files. Clones share the same data.
#[derive(Clone, Default)]
pub struct MemoryRepository {
    state: Arc<Mutex<State>>,
}

impl MemoryRepository {
    /// No repositories.
    pub fn new() -> MemoryRepository {
        MemoryRepository::default()
    }

    /// The work tree at `root` has `HEAD` at `commit` and `origin` at `origin`.
    pub fn set_head(&self, root: &Path, commit: &str, origin: Option<&str>) {
        let mut s = self.state.lock().unwrap();
        s.heads.insert(root.to_path_buf(), commit.to_string());
        match origin {
            Some(o) => s.origins.insert(root.to_path_buf(), o.to_string()),
            None => s.origins.remove(root),
        };
    }

    /// `commit` is reachable from a remote-tracking branch.
    pub fn push(&self, commit: &str) {
        self.state.lock().unwrap().pushed.insert(commit.to_string());
    }

    /// `commit` contains `path` with `data`.
    pub fn commit_file(&self, commit: &str, path: &str, data: &[u8]) {
        self.state
            .lock()
            .unwrap()
            .files
            .insert((commit.to_string(), path.to_string()), data.to_vec());
    }

    /// The work tree at `root` has (`true`) or has no uncommitted changes.
    pub fn set_uncommitted(&self, root: &Path, dirty: bool) {
        let mut s = self.state.lock().unwrap();
        if dirty {
            s.uncommitted.insert(root.to_path_buf());
        } else {
            s.uncommitted.remove(root);
        }
    }

    /// Makes every later query fail with `reason`; `None` heals it.
    pub fn fail(&self, reason: Option<&str>) {
        self.state.lock().unwrap().failing = reason.map(str::to_string);
    }

    fn check(&self) -> Result<std::sync::MutexGuard<'_, State>, RepositoryError> {
        let s = self.state.lock().unwrap();
        match &s.failing {
            Some(r) => Err(RepositoryError(r.clone())),
            None => Ok(s),
        }
    }
}

#[async_trait]
impl Repository for MemoryRepository {
    async fn head(&self, root: &Path) -> Result<Option<String>, RepositoryError> {
        Ok(self.check()?.heads.get(root).cloned())
    }

    async fn is_pushed(&self, _root: &Path, commit: &str) -> Result<bool, RepositoryError> {
        Ok(self.check()?.pushed.contains(commit))
    }

    async fn show(
        &self,
        _root: &Path,
        commit: &str,
        path: &str,
    ) -> Result<Option<Vec<u8>>, RepositoryError> {
        Ok(self
            .check()?
            .files
            .get(&(commit.to_string(), path.to_string()))
            .cloned())
    }

    async fn origin_url(&self, root: &Path) -> Result<Option<String>, RepositoryError> {
        Ok(self.check()?.origins.get(root).cloned())
    }

    async fn has_uncommitted(&self, root: &Path) -> Result<bool, RepositoryError> {
        Ok(self.check()?.uncommitted.contains(root))
    }
}
