use async_trait::async_trait;
use std::fmt;
use std::path::Path;

/// Why a repository query failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryError(pub String);

impl fmt::Display for RepositoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "repository: {}", self.0)
    }
}

impl std::error::Error for RepositoryError {}

/// The git facts the hub needs to place a commit order (spec 3.4, 3.7): the commit of the
/// work tree, whether it is pushed, the profile file as committed and the `origin` address.
/// Every `root` is the work tree root the CLI reported; the implementation runs `git` there
/// and never changes the repository.
#[async_trait]
pub trait Repository: Send + Sync {
    /// The commit `HEAD` points at, `None` without commits.
    async fn head(&self, root: &Path) -> Result<Option<String>, RepositoryError>;

    /// Whether `commit` is reachable from a remote-tracking branch.
    async fn is_pushed(&self, root: &Path, commit: &str) -> Result<bool, RepositoryError>;

    /// Content of `path` (relative, `/` separated) as committed in `commit`; `None` when the
    /// commit has no such file.
    async fn show(
        &self,
        root: &Path,
        commit: &str,
        path: &str,
    ) -> Result<Option<Vec<u8>>, RepositoryError>;

    /// Address of the `origin` remote (`git remote get-url origin`), `None` without one.
    async fn origin_url(&self, root: &Path) -> Result<Option<String>, RepositoryError>;
}
