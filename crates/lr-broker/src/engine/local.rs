use crate::policy::LocalUserEntry;
use std::collections::BTreeSet;

/// The client record of an admitted local IPC peer (spec 3.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalClient {
    /// Client key: `local:` and the OS user.
    pub key: String,
    /// Label from its `[[local_user]]` entry.
    pub label: String,
    /// Whether the OS user owns the broker (rights on its state directory): it sees every run
    /// and log and may cancel other clients' orders under `[cancel] others = "owner"`.
    pub owner: bool,
}

/// Decides which local IPC peers the broker serves; the OS user comes from the kernel.
pub trait LocalAdmission: Send + Sync {
    /// The client record of `os_user`, `None` when it may not use the broker.
    fn admit(&self, os_user: &str, users: &[LocalUserEntry]) -> Option<LocalClient>;
}

/// Admits the OS users named exactly in `[[local_user]]`, plus the broker owners given at
/// construction.
#[derive(Debug, Clone, Default)]
pub struct ExactLocalUsers {
    owners: BTreeSet<String>,
}

impl ExactLocalUsers {
    /// Admission with these OS users as broker owners.
    pub fn with_owners(owners: impl IntoIterator<Item = String>) -> ExactLocalUsers {
        ExactLocalUsers {
            owners: owners.into_iter().collect(),
        }
    }
}

impl LocalAdmission for ExactLocalUsers {
    fn admit(&self, os_user: &str, users: &[LocalUserEntry]) -> Option<LocalClient> {
        let listed = users.iter().find(|u| u.name == os_user);
        let owner = self.owners.contains(os_user);
        if listed.is_none() && !owner {
            return None;
        }
        Some(LocalClient {
            key: format!("local:{os_user}"),
            label: listed.map(|u| u.label.clone()).unwrap_or_default(),
            owner,
        })
    }
}

pub(crate) const LOCAL_RUNNER_PREFIX: &str = "local:";
