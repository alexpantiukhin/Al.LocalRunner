//! The file operations of the file sink, behind a narrow trait so rotation, retention and
//! I/O failures are tested without a disk. Only `lr-obs` uses it; it is not a role contract.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// What the file sink needs from a file system.
pub trait LogFs: Send + Sync {
    /// Creates a directory and its missing parents, private to the owner where the OS allows.
    fn create_dir_all(&self, dir: &Path) -> io::Result<()>;

    /// Opens a file for appending, creating it private to the owner where the OS allows;
    /// returns the writer and the current length.
    fn open_append(&self, path: &Path) -> io::Result<(Box<dyn Write + Send>, u64)>;

    /// Renames a file; fails if the target exists.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// Removes a file.
    fn remove_file(&self, path: &Path) -> io::Result<()>;

    /// Names of the files directly inside `dir`, sorted.
    fn list(&self, dir: &Path) -> io::Result<Vec<String>>;
}

/// The real file system. On Unix new directories get mode `0700` and new files `0600`; on
/// Windows they inherit the ACL of the state directory, which the installer restricts.
pub struct OsLogFs;

impl LogFs for OsLogFs {
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir)
    }

    fn open_append(&self, path: &Path) -> io::Result<(Box<dyn Write + Send>, u64)> {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        let len = file.metadata()?.len();
        Ok((Box::new(io::BufWriter::with_capacity(64 * 1024, file)), len))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        if to.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "target exists",
            ));
        }
        std::fs::rename(from, to)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                if let Some(name) = entry.file_name().to_str() {
                    names.push(name.to_string());
                }
            }
        }
        names.sort();
        Ok(names)
    }
}

#[derive(Default)]
struct State {
    files: BTreeMap<PathBuf, Vec<u8>>,
    failing: Option<String>,
    fail_rename: bool,
    fail_flush: bool,
    buffered: bool,
    flushes: u64,
    open: BTreeMap<PathBuf, usize>,
    max_open: BTreeMap<PathBuf, usize>,
}

/// A file system in memory for tests; clones share the same files.
#[derive(Clone, Default)]
pub struct MemoryLogFs {
    state: Arc<Mutex<State>>,
}

impl MemoryLogFs {
    /// No files.
    pub fn new() -> MemoryLogFs {
        MemoryLogFs::default()
    }

    /// Puts a file with `data`.
    pub fn put(&self, path: &Path, data: &[u8]) {
        self.lock().files.insert(path.to_path_buf(), data.to_vec());
    }

    /// Content of a file.
    pub fn file(&self, path: &Path) -> Option<Vec<u8>> {
        self.lock().files.get(path).cloned()
    }

    /// Names of the files directly inside `dir`, sorted.
    pub fn names(&self, dir: &Path) -> Vec<String> {
        self.lock()
            .files
            .keys()
            .filter(|p| p.parent() == Some(dir))
            .filter_map(|p| p.file_name()?.to_str().map(str::to_string))
            .collect()
    }

    /// Makes every operation fail with `reason`; `None` heals it.
    pub fn fail(&self, reason: Option<&str>) {
        self.lock().failing = reason.map(str::to_string);
    }

    /// Makes renames fail.
    pub fn fail_rename(&self, on: bool) {
        self.lock().fail_rename = on;
    }

    /// Makes files opened from now on buffer writes until `flush`, like a `BufWriter`: a
    /// failed flush or a dropped handle on a failing file system loses the buffered data.
    pub fn buffered(&self, on: bool) {
        self.lock().buffered = on;
    }

    /// Makes flushes fail (and lose what was buffered).
    pub fn fail_flush(&self, on: bool) {
        self.lock().fail_flush = on;
    }

    /// Most handles that were open at the same time on `path`.
    pub fn max_open(&self, path: &Path) -> usize {
        self.lock().max_open.get(path).copied().unwrap_or(0)
    }

    /// Flushes of open files so far.
    pub fn flushes(&self) -> u64 {
        self.lock().flushes
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn check(&self) -> io::Result<std::sync::MutexGuard<'_, State>> {
        let s = self.lock();
        match &s.failing {
            Some(reason) => Err(io::Error::other(reason.clone())),
            None => Ok(s),
        }
    }
}

struct MemoryFile {
    fs: MemoryLogFs,
    path: PathBuf,
    buffer: Option<Vec<u8>>,
}

impl Write for MemoryFile {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut s = self.fs.check()?;
        match self.buffer.as_mut() {
            Some(buffer) => buffer.extend_from_slice(data),
            None => s
                .files
                .entry(self.path.clone())
                .or_default()
                .extend_from_slice(data),
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let pending = self.buffer.as_mut().map(std::mem::take).unwrap_or_default();
        let mut s = self.fs.check()?;
        if s.fail_flush {
            return Err(io::Error::other("flush refused"));
        }
        s.flushes += 1;
        s.files
            .entry(self.path.clone())
            .or_default()
            .extend_from_slice(&pending);
        Ok(())
    }
}

impl Drop for MemoryFile {
    fn drop(&mut self) {
        if self.buffer.as_ref().is_some_and(|b| !b.is_empty()) {
            let _ = self.flush();
        }
        let mut s = self.fs.lock();
        if let Some(n) = s.open.get_mut(&self.path) {
            *n = n.saturating_sub(1);
        }
    }
}

impl LogFs for MemoryLogFs {
    fn create_dir_all(&self, _dir: &Path) -> io::Result<()> {
        self.check().map(|_| ())
    }

    fn open_append(&self, path: &Path) -> io::Result<(Box<dyn Write + Send>, u64)> {
        let mut s = self.check()?;
        let len = s.files.entry(path.to_path_buf()).or_default().len() as u64;
        let open = s.open.entry(path.to_path_buf()).or_default();
        *open += 1;
        let now_open = *open;
        let max = s.max_open.entry(path.to_path_buf()).or_default();
        *max = (*max).max(now_open);
        let buffer = s.buffered.then(Vec::new);
        drop(s);
        let file = MemoryFile {
            fs: self.clone(),
            path: path.to_path_buf(),
            buffer,
        };
        Ok((Box::new(file), len))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut s = self.check()?;
        if s.fail_rename {
            return Err(io::Error::other("rename refused"));
        }
        if s.files.contains_key(to) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "target exists",
            ));
        }
        let data = s
            .files
            .remove(from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))?;
        s.files.insert(to.to_path_buf(), data);
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.check()?
            .files
            .remove(path)
            .map(|_| ())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<String>> {
        drop(self.check()?);
        Ok(self.names(dir))
    }
}
