//! `init [--role hub|broker|runner|all]` (spec 10.2, 11.1): the state directory and commented
//! configuration templates of the chosen roles; existing files are never overwritten. The roles
//! are remembered in `roles` of the state directory (`autostart on` without a role, `up`).

use crate::paths::Layout;
use std::path::{Path, PathBuf};

/// File of the state directory listing the roles `init` created, one per line.
pub const ROLES_FILE: &str = "roles";

/// A role of this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NodeRole {
    /// The hub.
    Hub,
    /// The broker.
    Broker,
    /// Runners.
    Runner,
}

impl NodeRole {
    /// Its name in commands and in [`ROLES_FILE`].
    pub fn name(self) -> &'static str {
        match self {
            NodeRole::Hub => "hub",
            NodeRole::Broker => "broker",
            NodeRole::Runner => "runner",
        }
    }

    /// All roles.
    pub const ALL: [NodeRole; 3] = [NodeRole::Hub, NodeRole::Broker, NodeRole::Runner];

    fn parse(text: &str) -> Option<NodeRole> {
        NodeRole::ALL.into_iter().find(|r| r.name() == text.trim())
    }
}

/// `policy.toml` of a broker installed by `user`.
pub fn policy_template(user: &str) -> String {
    format!(
        "# Broker policy (spec 3.5). `localrunner policy check` validates this file and\n\
         # `localrunner policy reload` applies it; [listen] and [networks] apply at start only.\n\
         \n\
         # The broker listens on the network only on explicit addresses, and only while a join\n\
         # code is open or a machine has joined. Wildcard addresses are refused.\n\
         # [listen]\n\
         # address = \"192.168.1.10:7443\"\n\
         \n\
         # Networks that make the zone `lan`; addresses outside them are `remote`.\n\
         # [networks]\n\
         # home = [\"192.168.1.0/24\", \"127.0.0.1/32\"]\n\
         \n\
         # [join]\n\
         # code_ttl_min = 60\n\
         \n\
         # OS users allowed on the local IPC of this broker, each with an optional label.\n\
         [[local_user]]\n\
         name = {}\n",
        toml::Value::String(user.to_string())
    )
}

/// `hub-policy.toml` of a hub.
pub const HUB_POLICY_TEMPLATE: &str =
    "# Hub policy (spec 3.1): which known broker gets an order.\n\
\n\
# [brokers]\n\
# order = [\"office\"]          # names from `hub join --name`, in order of preference\n\
# strategy = \"least_loaded\"\n\
\n\
# [outbox]\n\
# ttl_hours = 24\n";

/// `runner.toml` template of the runners of this installation.
pub const RUNNER_TEMPLATE: &str =
    "# Runner settings (spec 3.10). Each runner copies this template into\n\
# runner/<name>/runner.toml when it joins; the file there wins.\n\
\n\
# name = \"linux-heavy\"          # default: --name, else the host name\n\
# platforms = [\"linux\"]\n\
# tags = []\n\
# mem_mb = 16384                 # default: all memory of the host\n\
# cpu = 8                        # default: all cores of the host\n\
# allow_elevated = false         # running as root or administrator needs true\n\
\n\
# [programs]\n\
# git = {}\n\
\n\
# [limits]\n\
# max_job_timeout_s = 3600\n";

/// `logging.toml` of every role.
pub const LOGGING_TEMPLATE: &str =
    "# Diagnostics of the roles (spec 12). Without sinks: a JSON file with rotation\n\
# under logs/. `localrunner logging reload` rereads it.\n\
\n\
# [logging]\n\
# level = \"info\"\n\
\n\
# [[sink]]\n\
# kind = \"file\"\n\
# path = \"logs/daemon.log\"\n";

/// What `init` did with one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    /// The file.
    pub path: PathBuf,
    /// `false` when it existed and was left as it is.
    pub written: bool,
}

/// The roles `init` created on this installation, in role order.
pub fn roles(layout: &Layout) -> Vec<NodeRole> {
    let text = std::fs::read_to_string(layout.root.join(ROLES_FILE)).unwrap_or_default();
    let mut out: Vec<NodeRole> = text.lines().filter_map(NodeRole::parse).collect();
    out.sort();
    out.dedup();
    out
}

fn write_new(path: PathBuf, contents: &str) -> Result<Created, String> {
    if path.exists() {
        return Ok(Created {
            path,
            written: false,
        });
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Created {
        path,
        written: true,
    })
}

/// Makes the state directory reachable for its owner only (spec 10.3): mode 0700 on Unix; on
/// Windows the user's profile directory already is private.
pub fn close_to_owner(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// The state directory `dir`, created when missing and closed to its owner (spec 10.3); the
/// broker service's directory keeps the modes `broker install-service` gave it.
pub fn ensure_state_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if dir == crate::paths::service_root() {
        return Ok(());
    }
    close_to_owner(dir)
}

/// Creates the state directory (closed to its owner), `machine-id` and the templates of
/// `roles` for OS user `user`.
pub fn init(layout: &Layout, roles: &[NodeRole], user: &str) -> Result<Vec<Created>, String> {
    crate::init::ensure_state_dir(&layout.root)?;
    crate::host::machine_id(&layout.root)?;
    let mut out = vec![write_new(
        layout.config_file("logging.toml"),
        LOGGING_TEMPLATE,
    )?];
    for role in roles {
        out.push(match role {
            NodeRole::Broker => {
                write_new(layout.config_file("policy.toml"), &policy_template(user))?
            }
            NodeRole::Hub => write_new(layout.config_file("hub-policy.toml"), HUB_POLICY_TEMPLATE)?,
            NodeRole::Runner => write_new(layout.config_file("runner.toml"), RUNNER_TEMPLATE)?,
        });
    }
    let mut all = self::roles(layout);
    all.extend_from_slice(roles);
    all.sort();
    all.dedup();
    let text: String = all.iter().map(|r| format!("{}\n", r.name())).collect();
    let path = layout.root.join(ROLES_FILE);
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(out)
}
