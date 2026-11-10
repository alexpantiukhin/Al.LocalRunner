//! Where a node keeps its state and configuration (spec 10.3) and the local IPC addresses of
//! its roles (spec 10, table "IPC").

use std::path::PathBuf;

/// Environment variable naming the state directory.
pub const STATE_DIR_ENV: &str = "LOCALRUNNER_STATE_DIR";
/// Environment variable naming the configuration directory (spec 10.4).
pub const CONFIG_DIR_ENV: &str = "LOCALRUNNER_CONFIG_DIR";

const PIPE_PREFIX: &str = r"\\.\pipe\al-localrunner";

/// The state and configuration directories of one installation and the IPC addresses derived
/// from them. Roles of the default installation use the fixed IPC names of the spec; any other
/// state directory gets names with a hash of its path, so two installations never meet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// State directory: `machine-id`, databases, keys, `logs/`, `runner/<name>/`.
    pub root: PathBuf,
    /// Configuration directory: `policy.toml`, `hub-policy.toml`, `logging.toml`,
    /// `runner.toml` templates; the state directory unless given.
    pub config: PathBuf,
    suffix: String,
}

fn fnv64(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The default state directory of the current OS user (spec 10.3).
pub fn default_root() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Al.LocalRunner"))
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/al-localrunner"))
    }
}

impl Layout {
    /// A layout over explicit directories.
    pub fn new(root: PathBuf, config: PathBuf) -> Layout {
        let suffix = if Some(&root) == default_root().as_ref() {
            String::new()
        } else {
            format!("-{:016x}", fnv64(&root.to_string_lossy()))
        };
        Layout {
            root,
            config,
            suffix,
        }
    }

    /// The layout from flags, then [`STATE_DIR_ENV`] and [`CONFIG_DIR_ENV`], then the default.
    pub fn resolve(state: Option<PathBuf>, config: Option<PathBuf>) -> Result<Layout, String> {
        let env = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let root = state
            .or_else(|| env(STATE_DIR_ENV))
            .or_else(default_root)
            .ok_or("no state directory: set --state-dir or LOCALRUNNER_STATE_DIR")?;
        let config = config
            .or_else(|| env(CONFIG_DIR_ENV))
            .unwrap_or_else(|| root.clone());
        Ok(Layout::new(root, config))
    }

    /// A configuration file of the installation.
    pub fn config_file(&self, name: &str) -> PathBuf {
        self.config.join(name)
    }

    /// State directory of runner `name`.
    pub fn runner_dir(&self, name: &str) -> PathBuf {
        self.root.join("runner").join(name)
    }

    /// Names of the runners created on this installation, sorted.
    pub fn runner_names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.root.join("runner"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();
        names
    }

    fn ipc(&self, name: &str) -> String {
        if cfg!(windows) {
            format!("{PIPE_PREFIX}-{name}{}", self.suffix)
        } else {
            self.root
                .join("ipc")
                .join(format!("{name}.sock"))
                .to_string_lossy()
                .into_owned()
        }
    }

    /// The CLI channel of the hub of OS principal `principal` (SID on Windows).
    pub fn hub_ipc(&self, principal: &str) -> String {
        self.ipc(&format!("hub-{principal}"))
    }

    /// The broker's IPC for hubs and runners of this machine.
    pub fn broker_ipc(&self) -> String {
        self.ipc("broker")
    }

    /// The broker's administrative channel (spec 3.9): local IPC, broker owner only.
    pub fn admin_ipc(&self) -> String {
        self.ipc("broker-admin")
    }

    /// The control channel of runner `name` of OS principal `principal`.
    pub fn runner_ipc(&self, principal: &str, name: &str) -> String {
        self.ipc(&format!("runner-{principal}-{name}"))
    }

    /// Creates the directory of Unix sockets, if this OS uses them.
    pub fn prepare_ipc(&self) -> std::io::Result<()> {
        if cfg!(windows) {
            return Ok(());
        }
        std::fs::create_dir_all(self.root.join("ipc"))
    }
}
