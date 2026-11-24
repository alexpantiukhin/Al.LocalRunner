use super::error::{PolicyError, PolicyWarning};
use super::file::{load, HostResources, PolicyFile};
use super::into_policy;
use super::validate::check;
use lr_core::Policy;
use lr_proto::SettingsUpdate;

/// What a successful `policy reload` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// Warnings of the new file; they did not stop it from applying.
    pub warnings: Vec<PolicyWarning>,
    /// `Some` when `[grace]` changed: the broker sends it to every connected runner.
    pub settings_update: Option<SettingsUpdate>,
}

/// The policy the broker runs with: the applied file and the core policy built from it.
pub struct ActivePolicy {
    file: PolicyFile,
    policy: Box<dyn Policy + Send + Sync>,
    host: HostResources,
    in_container: bool,
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
    /// Loads the file at service start; `[listen]`, `[networks]` and `[[shared]]` take effect now.
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
        Ok((
            ActivePolicy {
                file,
                policy,
                host,
                in_container,
            },
            warnings,
        ))
    }

    /// The applied file.
    pub fn file(&self) -> &PolicyFile {
        &self.file
    }

    /// The core policy of the applied file.
    pub fn policy(&self) -> &(dyn Policy + Send + Sync) {
        self.policy.as_ref()
    }

    /// `policy reload`: an invalid file is refused and the previous policy stays. A valid one
    /// applies at once, `[listen]`, `[networks]` and `[[shared]]` included; the assembly
    /// rebinds the network listener to the new `[listen]` before it calls this.
    pub fn reload(&mut self, text: &str) -> Result<Applied, PolicyError> {
        let file = load(text)?;
        file.listen.check_at_start(self.in_container)?;
        let warnings = check(&file, &self.host);
        let settings_update = (file.grace != self.file.grace).then(|| file.settings_update());
        self.policy = into_policy(&file, &self.host);
        self.file = file;
        Ok(Applied {
            warnings,
            settings_update,
        })
    }
}
