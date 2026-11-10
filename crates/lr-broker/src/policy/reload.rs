use super::error::{PolicyError, PolicyWarning};
use super::file::{load, HostResources, PolicyFile};
use super::into_policy;
use super::validate::check;
use lr_core::Policy;
use lr_proto::SettingsUpdate;

/// A table that `policy reload` reads but applies only when the service restarts (spec 3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartOnly {
    /// `[listen]`.
    Listen,
    /// `[networks]`.
    Networks,
}

/// What a successful `policy reload` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// Warnings of the new file; they did not stop it from applying.
    pub warnings: Vec<PolicyWarning>,
    /// Tables whose new values differ from the running ones and wait for a restart.
    pub restart_required: Vec<RestartOnly>,
    /// `Some` when `[grace]` changed: the broker sends it to every connected runner.
    pub settings_update: Option<SettingsUpdate>,
}

/// The policy the broker runs with: the applied file and the core policy built from it.
pub struct ActivePolicy {
    file: PolicyFile,
    policy: Box<dyn Policy + Send + Sync>,
    host: HostResources,
}

impl std::fmt::Debug for ActivePolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivePolicy")
            .field("file", &self.file)
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

impl ActivePolicy {
    /// Loads the file at service start; `[listen]` and `[networks]` take effect now.
    /// `in_container` tells whether the service runs inside a container, where alone
    /// `address = "container"` is accepted.
    pub fn start(
        text: &str,
        host: HostResources,
        in_container: bool,
    ) -> Result<(ActivePolicy, Vec<PolicyWarning>), PolicyError> {
        let file = load(text)?;
        file.listen.check_at_start(in_container)?;
        let warnings = check(&file, &host);
        let policy = into_policy(&file, &host);
        Ok((ActivePolicy { file, policy, host }, warnings))
    }

    /// The applied file. Its `[listen]` and `[networks]` are those of the start.
    pub fn file(&self) -> &PolicyFile {
        &self.file
    }

    /// The core policy of the applied file.
    pub fn policy(&self) -> &(dyn Policy + Send + Sync) {
        self.policy.as_ref()
    }

    /// `policy reload`: an invalid file is refused and the previous policy stays. A valid one
    /// applies at once, except `[listen]` and `[networks]`, which keep their start values and
    /// are reported while the file differs from them.
    pub fn reload(&mut self, text: &str) -> Result<Applied, PolicyError> {
        let mut file = load(text)?;
        let warnings = check(&file, &self.host);
        let mut restart_required = Vec::new();
        if file.listen != self.file.listen {
            restart_required.push(RestartOnly::Listen);
        }
        if file.networks != self.file.networks {
            restart_required.push(RestartOnly::Networks);
        }
        file.listen = self.file.listen.clone();
        file.networks = self.file.networks.clone();
        let settings_update = (file.grace != self.file.grace).then(|| file.settings_update());
        self.policy = into_policy(&file, &self.host);
        self.file = file;
        Ok(Applied {
            warnings,
            restart_required,
            settings_update,
        })
    }
}
