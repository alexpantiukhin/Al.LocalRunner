//! `hub join` and `runner join` (spec 3.8, 11.1): the joiner creates its key pair, proves it
//! over TLS pinned to the fingerprint inside the code, and keeps the broker's address.

use crate::build::{BROKER_FILE, KEY_FILE};
use crate::join::{join, Joiner};
use crate::paths::Layout;
use crate::runner_config::{check_runner_name, instance_names, RunnerConfig};
use lr_abstractions::{BrokerLink, HubStore};
use lr_net::ClientKey;
use lr_proto::Role;
use lr_store::SqliteHubStore;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Variable that can carry the join code instead of the command line.
pub const JOIN_CODE_ENV: &str = "LOCALRUNNER_JOIN_CODE";

/// Where the join code came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeFrom {
    /// `--code-file` (a file, or `-` for standard input).
    File,
    /// [`JOIN_CODE_ENV`].
    Env,
    /// The command line, where other local users can see it while the join runs.
    Argv,
}

/// The join code by priority: `--code-file`, then [`JOIN_CODE_ENV`], then the command line;
/// blank values do not count.
pub fn pick_code(
    file: Option<String>,
    env: Option<String>,
    argv: Option<String>,
) -> Option<(String, CodeFrom)> {
    let given = |v: Option<String>| v.map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
    given(file)
        .map(|c| (c, CodeFrom::File))
        .or_else(|| given(env).map(|c| (c, CodeFrom::Env)))
        .or_else(|| given(argv).map(|c| (c, CodeFrom::Argv)))
}

fn read_stdin_line(prompt: bool) -> Result<String, String> {
    use std::io::{BufRead, IsTerminal, Write};
    if prompt && std::io::stdin().is_terminal() {
        eprint!("join code: ");
        let _ = std::io::stderr().flush();
    }
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("standard input: {e}"))?;
    Ok(line)
}

/// The join code of `hub join` or `runner join` (spec 3.8): from `--code-file` (`-` reads
/// standard input), [`JOIN_CODE_ENV`] or the command line (with a warning); without any, it
/// is read from standard input.
pub fn join_code(argv: Option<String>, file: Option<PathBuf>) -> Result<String, String> {
    let from_file = match &file {
        Some(p) if p.as_os_str() == "-" => Some(read_stdin_line(false)?),
        Some(p) => Some(std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?),
        None => None,
    };
    let env = std::env::var(JOIN_CODE_ENV).ok();
    match pick_code(from_file, env, argv) {
        Some((code, from)) => {
            if from == CodeFrom::Argv {
                eprintln!(
                    "warning: a code on the command line is visible to other users of this \
                     machine and stays in the shell history; use --code-file or {JOIN_CODE_ENV}"
                );
            }
            Ok(code)
        }
        None => pick_code(Some(read_stdin_line(true)?), None, None)
            .map(|(c, _)| c)
            .ok_or_else(|| format!("no join code: give --code-file, {JOIN_CODE_ENV} or type it")),
    }
}

/// The participant key at `path`, created on first use.
pub fn key_at(path: &Path) -> Result<Arc<ClientKey>, String> {
    let key = if path.exists() {
        ClientKey::load(path)
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        ClientKey::generate().and_then(|k| k.save(path).map(|()| k))
    };
    key.map(Arc::new)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// `hub join <address> <code> [--name broker-name]`: the broker goes into this hub's list
/// under `name` (default: the address).
pub async fn hub_join(
    layout: &Layout,
    address: &str,
    code: &str,
    name: Option<String>,
) -> Result<i32, String> {
    crate::init::ensure_state_dir(&layout.root)?;
    let key = key_at(&layout.root.join(KEY_FILE))?;
    let who = Joiner {
        role: Role::Hub,
        name: crate::facts::host_name(),
        machine_id: crate::host::machine_id(&layout.root)?,
        capabilities: None,
    };
    crate::state_dir::print_warnings();
    let (joined, fingerprint) = join(address, code, key, who).await?;
    let broker = name.unwrap_or_else(|| address.to_string());
    let store = SqliteHubStore::open(&layout.root.join("hub.db")).map_err(|e| e.to_string())?;
    store
        .upsert_broker(BrokerLink {
            name: broker.clone(),
            address: address.to_string(),
            fingerprint,
            key_id: joined.key_id,
            machine_id: joined.broker_machine_id,
            last_zone: joined.zone,
        })
        .await
        .map_err(|e| e.to_string())?;
    println!("joined broker {broker} at {address}; a running hub connects to it after a restart");
    Ok(0)
}

/// What `runner join` was given.
#[derive(Debug, Clone)]
pub struct RunnerJoin {
    /// `host:port` of the broker.
    pub address: String,
    /// The one-time code.
    pub code: String,
    /// Runner name.
    pub name: Option<String>,
    /// Number of runners.
    pub count: u32,
    /// `runner.toml` template.
    pub config: Option<PathBuf>,
    /// Platforms replacing the template's.
    pub platforms: Vec<String>,
    /// Tags replacing the template's.
    pub tags: Vec<String>,
}

fn template(layout: &Layout, j: &RunnerJoin) -> Result<String, String> {
    let path = j
        .config
        .clone()
        .unwrap_or_else(|| layout.config_file("runner.toml"));
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) if j.config.is_none() => {
            crate::init::runner_template(std::env::var("PATH").ok().as_deref())
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if j.platforms.is_empty() && j.tags.is_empty() {
        return Ok(text);
    }
    let mut table: toml::Table = toml::from_str(&text).map_err(|e| e.message().to_string())?;
    let list =
        |v: &[String]| toml::Value::Array(v.iter().cloned().map(toml::Value::String).collect());
    if !j.platforms.is_empty() {
        table.insert("platforms".into(), list(&j.platforms));
    }
    if !j.tags.is_empty() {
        table.insert("tags".into(), list(&j.tags));
    }
    toml::to_string(&table).map_err(|e| e.to_string())
}

/// `runner join <address> <code> [--name] [--count N] [--config] [--platforms] [--tags]`:
/// each runner gets its own directory, `runner.toml` from the template, key and record at the
/// broker; `broker.toml` keeps the address and the pinned fingerprint.
pub async fn runner_join(layout: &Layout, j: RunnerJoin) -> Result<i32, String> {
    let base = crate::roles::runner_base_name(layout, j.name.as_deref(), j.config.as_deref());
    let names = instance_names(&base, j.count)?;
    let text = template(layout, &j)?;
    let host = crate::host::resources();
    crate::init::ensure_state_dir(&layout.root)?;
    let machine_id = crate::host::machine_id(&layout.root)?;
    crate::state_dir::print_warnings();
    for name in &names {
        check_runner_name(name)?;
        let dir = layout.runner_dir(name);
        if dir.join(BROKER_FILE).exists() {
            return Err(format!(
                "runner {name} has joined a broker already ({})",
                dir.join(BROKER_FILE).display()
            ));
        }
    }
    for name in names {
        let dir = layout.runner_dir(&name);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let own = dir.join("runner.toml");
        if !own.exists() {
            std::fs::write(&own, &text).map_err(|e| format!("{}: {e}", own.display()))?;
        }
        let own_text =
            std::fs::read_to_string(&own).map_err(|e| format!("{}: {e}", own.display()))?;
        let mut cfg = RunnerConfig::parse(&own_text, Some(&name), &name, dir.clone(), host)
            .map_err(|e| format!("runner {name}: {e}"))?;
        for p in cfg.check_programs() {
            eprintln!("runner {name}: {p}");
        }
        cfg.settings.disk_mb = crate::host::free_disk_mb(&dir).unwrap_or(0);
        let who = Joiner {
            role: Role::Runner,
            name: name.clone(),
            machine_id: machine_id.clone(),
            capabilities: Some(cfg.settings.capabilities(None)),
        };
        let key = key_at(&dir.join(KEY_FILE))?;
        let (_, fingerprint) = join(&j.address, &j.code, key, who).await?;
        let mut file = toml::Table::new();
        file.insert("address".into(), toml::Value::String(j.address.clone()));
        file.insert("fingerprint".into(), toml::Value::String(fingerprint));
        let path = dir.join(BROKER_FILE);
        let body = toml::to_string(&file).map_err(|e| e.to_string())?;
        std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))?;
        println!(
            "runner {name} joined {}; start it: localrunner runner run --name {name}",
            j.address
        );
    }
    Ok(0)
}
