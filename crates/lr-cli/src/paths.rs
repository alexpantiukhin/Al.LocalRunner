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
    ipc_dir: PathBuf,
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

/// The state directory of the broker service (spec 10.3).
pub fn service_root() -> PathBuf {
    if cfg!(windows) {
        let data = std::env::var_os("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into());
        PathBuf::from(data).join("Al.LocalRunner")
    } else if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/Al.LocalRunner")
    } else {
        PathBuf::from("/var/lib/al-localrunner")
    }
}

/// Where the broker service keeps its Unix sockets (spec 10, table "IPC"): `/run/al-localrunner`
/// on Linux (created by systemd for the service), `ipc/` of the service state directory on
/// macOS; unused on Windows, whose pipe names are global.
pub fn service_ipc_dir() -> PathBuf {
    if cfg!(target_os = "linux") {
        PathBuf::from("/run/al-localrunner")
    } else {
        service_root().join("ipc")
    }
}

impl Layout {
    /// A layout over explicit directories. The default directory of the user and the service
    /// directory use the fixed IPC names, so a hub finds the broker service by them.
    pub fn new(root: PathBuf, config: PathBuf) -> Layout {
        let service = root == service_root();
        let suffix = if service || Some(&root) == default_root().as_ref() {
            String::new()
        } else {
            format!("-{:016x}", fnv64(&root.to_string_lossy()))
        };
        let ipc_dir = if service {
            service_ipc_dir()
        } else {
            root.join("ipc")
        };
        Layout {
            root,
            config,
            suffix,
            ipc_dir,
        }
    }

    /// The layout of the broker service.
    pub fn service() -> Layout {
        Layout::new(service_root(), service_root())
    }

    /// Whether this is the broker service's layout.
    pub fn is_service(&self) -> bool {
        self.root == service_root()
    }

    /// Whether the broker this layout reaches is the OS service: the service's own layout, or
    /// the default layout of a user (which uses the service's IPC names) on a machine where
    /// the broker service is installed.
    pub fn broker_is_service(&self) -> bool {
        self.is_service()
            || (self.suffix.is_empty()
                && crate::os_service::service_installed(crate::os_service::Os::current()))
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
            self.ipc_dir
                .join(format!("{name}.sock"))
                .to_string_lossy()
                .into_owned()
        }
    }

    fn broker_side(&self, name: &str) -> String {
        if !cfg!(windows) && self.suffix.is_empty() && !self.is_service() {
            let service = service_ipc_dir().join(format!("{name}.sock"));
            if service.exists() {
                return service.to_string_lossy().into_owned();
            }
        }
        self.ipc(name)
    }

    /// The CLI channel of the hub of OS principal `principal` (SID on Windows).
    pub fn hub_ipc(&self, principal: &str) -> String {
        self.ipc(&format!("hub-{principal}"))
    }

    /// The broker's IPC for hubs and runners of this machine; for the default layout of a
    /// user on Unix, the broker service's socket when the service runs.
    pub fn broker_ipc(&self) -> String {
        self.broker_side("broker")
    }

    /// The broker's administrative channel (spec 3.9): local IPC, broker owner and
    /// administrators only.
    pub fn admin_ipc(&self) -> String {
        self.broker_side("broker-admin")
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
        std::fs::create_dir_all(&self.ipc_dir).map_err(|e| {
            let hint = if e.kind() == std::io::ErrorKind::PermissionDenied {
                format!(
                    "; the directory must exist and be writable by the user running localrunner (for the official image: `mkdir -p {0} && chown <user> {0}` in the Dockerfile)",
                    self.ipc_dir.display()
                )
            } else {
                String::new()
            };
            std::io::Error::new(
                e.kind(),
                format!("cannot create {}: {e}{hint}", self.ipc_dir.display()),
            )
        })
    }
}
