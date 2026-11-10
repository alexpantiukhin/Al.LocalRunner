use async_trait::async_trait;
use std::fmt;
use std::path::Path;

/// Why a file operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesError(pub String);

impl fmt::Display for FilesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "file system: {}", self.0)
    }
}

impl std::error::Error for FilesError {}

/// The few file operations a runner needs in its state directory: workspace and cache
/// directories, their sizes, and small state files. Programs are never started through it.
#[async_trait]
pub trait Files: Send + Sync {
    /// Whether a file or directory exists at `path`.
    async fn exists(&self, path: &Path) -> Result<bool, FilesError>;

    /// Creates the directory and every missing parent.
    async fn create_dir_all(&self, path: &Path) -> Result<(), FilesError>;

    /// Removes the directory with everything inside; a missing directory is not an error.
    async fn remove_dir_all(&self, path: &Path) -> Result<(), FilesError>;

    /// Total size in bytes of the files under `path`, 0 when it does not exist.
    async fn size(&self, path: &Path) -> Result<u64, FilesError>;

    /// The content of a small file, `None` when it does not exist.
    async fn read(&self, path: &Path) -> Result<Option<Vec<u8>>, FilesError>;

    /// Replaces a small file as a whole: a reader sees the old or the new content, never a
    /// mix. Missing parents are created.
    async fn write(&self, path: &Path, data: &[u8]) -> Result<(), FilesError>;

    /// Removes a file; a missing file is not an error.
    async fn remove_file(&self, path: &Path) -> Result<(), FilesError>;
}
