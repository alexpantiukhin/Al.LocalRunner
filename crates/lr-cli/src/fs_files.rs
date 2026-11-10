//! [`Files`] on the real file system; every call runs on the blocking pool.

use async_trait::async_trait;
use lr_abstractions::{Files, FilesError};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The file system of this machine.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdFiles;

fn err(path: &Path, e: std::io::Error) -> FilesError {
    FilesError(format!("{}: {e}", path.display()))
}

async fn blocking<T: Send + 'static>(
    path: &Path,
    f: impl FnOnce(PathBuf) -> Result<T, FilesError> + Send + 'static,
) -> Result<T, FilesError> {
    let owned = path.to_path_buf();
    tokio::task::spawn_blocking(move || f(owned))
        .await
        .map_err(|e| FilesError(format!("{}: {e}", path.display())))?
}

fn tree_size(path: &Path) -> std::io::Result<u64> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    if !meta.is_dir() {
        return Ok(meta.len());
    }
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}

fn temp_name(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp-{}-{n}", std::process::id()));
    path.with_file_name(name)
}

fn write_synced(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(data)?;
    file.sync_all()
}

#[async_trait]
impl Files for StdFiles {
    async fn exists(&self, path: &Path) -> Result<bool, FilesError> {
        blocking(path, |p| p.try_exists().map_err(|e| err(&p, e))).await
    }

    async fn create_dir_all(&self, path: &Path) -> Result<(), FilesError> {
        blocking(path, |p| {
            std::fs::create_dir_all(&p).map_err(|e| err(&p, e))
        })
        .await
    }

    async fn remove_dir_all(&self, path: &Path) -> Result<(), FilesError> {
        blocking(path, |p| match std::fs::remove_dir_all(&p) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(err(&p, e)),
            _ => Ok(()),
        })
        .await
    }

    async fn size(&self, path: &Path) -> Result<u64, FilesError> {
        blocking(path, |p| tree_size(&p).map_err(|e| err(&p, e))).await
    }

    async fn read(&self, path: &Path) -> Result<Option<Vec<u8>>, FilesError> {
        blocking(path, |p| match std::fs::read(&p) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(err(&p, e)),
        })
        .await
    }

    async fn write(&self, path: &Path, data: &[u8]) -> Result<(), FilesError> {
        let data = data.to_vec();
        blocking(path, move |p| {
            if let Some(parent) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|e| err(parent, e))?;
            }
            let tmp = temp_name(&p);
            if let Err(e) = write_synced(&tmp, &data) {
                let _ = std::fs::remove_file(&tmp);
                return Err(err(&tmp, e));
            }
            std::fs::rename(&tmp, &p).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                err(&p, e)
            })
        })
        .await
    }

    async fn remove_file(&self, path: &Path) -> Result<(), FilesError> {
        blocking(path, |p| match std::fs::remove_file(&p) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(err(&p, e)),
            _ => Ok(()),
        })
        .await
    }
}
