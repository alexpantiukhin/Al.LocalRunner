use crate::files::{Files, FilesError};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Dir,
    File(Vec<u8>),
}

#[derive(Default)]
struct State {
    nodes: BTreeMap<PathBuf, Node>,
    failing: Option<String>,
}

/// A file system in memory: directories and files by path. Clones share the same tree.
#[derive(Clone, Default)]
pub struct MemoryFiles {
    state: Arc<Mutex<State>>,
}

impl MemoryFiles {
    /// An empty tree.
    pub fn new() -> MemoryFiles {
        MemoryFiles::default()
    }

    /// Puts a file with `data`, creating its parents.
    pub fn put(&self, path: &Path, data: &[u8]) {
        let mut s = self.state.lock().unwrap();
        add_parents(&mut s.nodes, path);
        s.nodes
            .insert(path.to_path_buf(), Node::File(data.to_vec()));
    }

    /// Creates a directory and its parents.
    pub fn mkdir(&self, path: &Path) {
        let mut s = self.state.lock().unwrap();
        add_parents(&mut s.nodes, path);
        s.nodes.insert(path.to_path_buf(), Node::Dir);
    }

    /// Whether a directory exists at `path`.
    pub fn is_dir(&self, path: &Path) -> bool {
        self.state.lock().unwrap().nodes.get(path) == Some(&Node::Dir)
    }

    /// Content of the file at `path`.
    pub fn file(&self, path: &Path) -> Option<Vec<u8>> {
        match self.state.lock().unwrap().nodes.get(path) {
            Some(Node::File(d)) => Some(d.clone()),
            _ => None,
        }
    }

    /// Makes every later operation fail with `reason`; `None` heals it.
    pub fn fail(&self, reason: Option<&str>) {
        self.state.lock().unwrap().failing = reason.map(str::to_string);
    }

    fn check(&self) -> Result<std::sync::MutexGuard<'_, State>, FilesError> {
        let s = self.state.lock().unwrap();
        match &s.failing {
            Some(r) => Err(FilesError(r.clone())),
            None => Ok(s),
        }
    }
}

fn add_parents(nodes: &mut BTreeMap<PathBuf, Node>, path: &Path) {
    let mut cur = path.parent();
    while let Some(p) = cur {
        if p.as_os_str().is_empty() {
            break;
        }
        nodes.entry(p.to_path_buf()).or_insert(Node::Dir);
        cur = p.parent();
    }
}

#[async_trait]
impl Files for MemoryFiles {
    async fn exists(&self, path: &Path) -> Result<bool, FilesError> {
        Ok(self.check()?.nodes.contains_key(path))
    }

    async fn create_dir_all(&self, path: &Path) -> Result<(), FilesError> {
        let mut s = self.check()?;
        if let Some(Node::File(_)) = s.nodes.get(path) {
            return Err(FilesError(format!("{} is a file", path.display())));
        }
        add_parents(&mut s.nodes, path);
        s.nodes.insert(path.to_path_buf(), Node::Dir);
        Ok(())
    }

    async fn remove_dir_all(&self, path: &Path) -> Result<(), FilesError> {
        let mut s = self.check()?;
        s.nodes.retain(|p, _| !p.starts_with(path));
        Ok(())
    }

    async fn size(&self, path: &Path) -> Result<u64, FilesError> {
        let s = self.check()?;
        Ok(s.nodes
            .iter()
            .filter(|(p, _)| p.starts_with(path))
            .map(|(_, n)| match n {
                Node::File(d) => d.len() as u64,
                Node::Dir => 0,
            })
            .sum())
    }

    async fn read(&self, path: &Path) -> Result<Option<Vec<u8>>, FilesError> {
        match self.check()?.nodes.get(path) {
            Some(Node::File(d)) => Ok(Some(d.clone())),
            Some(Node::Dir) => Err(FilesError(format!("{} is a directory", path.display()))),
            None => Ok(None),
        }
    }

    async fn write(&self, path: &Path, data: &[u8]) -> Result<(), FilesError> {
        let mut s = self.check()?;
        add_parents(&mut s.nodes, path);
        s.nodes
            .insert(path.to_path_buf(), Node::File(data.to_vec()));
        Ok(())
    }

    async fn remove_file(&self, path: &Path) -> Result<(), FilesError> {
        let mut s = self.check()?;
        if let Some(Node::File(_)) = s.nodes.get(path) {
            s.nodes.remove(path);
        }
        Ok(())
    }

    async fn list_files(&self, dir: &Path) -> Result<Vec<String>, FilesError> {
        let s = self.check()?;
        Ok(s.nodes
            .iter()
            .filter(|(p, n)| matches!(n, Node::File(_)) && p.parent() == Some(dir))
            .filter_map(|(p, _)| p.file_name()?.to_str().map(str::to_string))
            .collect())
    }
}
