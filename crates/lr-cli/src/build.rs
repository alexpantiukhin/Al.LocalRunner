//! The assembly (spec 13.1): the only place that creates the implementations of `lr-net`,
//! `lr-store`, `lr-exec` and `lr-obs` and hands them to the roles.

use crate::broker_admin::{ipc_access, listen_addresses, BrokerAdmin, TlsListening};
use crate::fs_files::StdFiles;
use crate::git_repo::GitRepository;
use crate::host;
use crate::net::{MergedListener, NetAdmission, RoutingDialer};
use crate::paths::Layout;
use crate::runner_config::{check_runner_name, elevation_allowed, RunnerConfig};
use crate::secrets::ConfigSecrets;
use lr_abstractions::{Clock, Dialer, Listener, Role};
use lr_broker::policy::{ActivePolicy, HostResources};
use lr_broker::{Broker, BrokerDeps, BrokerHandle};
use lr_exec::{ExecRunner, OsProcessInspector, SystemClock};
use lr_hub::{parse_hub_policy, Hub, HubDeps};
use lr_net::{
    current_os_principal, current_os_user, BrokerIdentity, ClientKey, IpcAccess, IpcDialer,
    IpcListener, Registry, TlsDialer,
};
use lr_obs::logger::Resource;
use lr_obs::{Logger, LoggingConfig, Metrics, ObsEnv, RedactingSecrets};
use lr_runner::{BrokerAddress, RunnerAgent, RunnerDeps};
use lr_store::{SqliteBrokerStore, SqliteHubStore, SqliteRunnerStore};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Key file of a hub or runner that joined a broker over the network.
pub const KEY_FILE: &str = "participant.key";
/// Address and pinned fingerprint of the broker a runner joined (`address`, `fingerprint`).
pub const BROKER_FILE: &str = "broker.toml";
/// Directory of the state root where the runners of the installation record the disk their
/// running jobs reserved (`<runner>.mb`).
pub const DISK_LEDGER: &str = "disk-reservations";

fn read_optional(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// The real clock.
pub fn clock() -> Arc<dyn Clock> {
    Arc::new(SystemClock)
}

/// The logger of a role from `logging.toml` of the installation; its files live under
/// `state_dir`. Workers are started.
pub fn logger(
    layout: &Layout,
    state_dir: &Path,
    role: Role,
    clock: Arc<dyn Clock>,
    foreground: bool,
) -> Result<Arc<Logger>, String> {
    let machine_id = host::machine_id(&layout.root)?;
    let text = read_optional(&layout.config_file("logging.toml"))?;
    let config = LoggingConfig::load(text.as_deref()).map_err(|e| e.to_string())?;
    let now = clock.now();
    let metrics = Arc::new(Metrics::new(now, config.client_label));
    let env = ObsEnv::system(
        state_dir.to_path_buf(),
        clock,
        metrics,
        Resource { role, machine_id },
        foreground,
    );
    let logger = Logger::new(&config, env).map_err(|e| e.to_string())?;
    logger.start_workers();
    for warning in crate::state_dir::take_warnings() {
        use lr_abstractions::{Level, LogSink, Record};
        logger.write(&Record::new(now, Level::Warn, role, "state_dir", &warning));
    }
    Ok(Arc::new(logger))
}

/// Rereads `path` (`logging.toml`; a missing file means the defaults) into `logger`; `Err`
/// names why the file was refused, and the running configuration stays.
pub fn reload_logging(path: &Path, logger: &Logger) -> Result<(), String> {
    let text = read_optional(path)?;
    LoggingConfig::load(text.as_deref())
        .and_then(|c| logger.reload(&c))
        .map_err(|e| e.to_string())
}

/// [`reload_logging`] of `logging.toml` of the installation, for a role's local channel.
pub fn logging_reloader(layout: &Layout, logger: &Arc<Logger>) -> lr_hub::ReloadLogging {
    let (path, logger) = (layout.config_file("logging.toml"), logger.clone());
    Arc::new(move || reload_logging(&path, &logger))
}

fn ipc_dialer() -> Result<Arc<dyn Dialer>, String> {
    Ok(Arc::new(IpcDialer::new(
        crate::os_service::broker_servers()?
    )))
}

fn dialer(key: &Path) -> Result<Arc<dyn Dialer>, String> {
    let tls: Option<Arc<dyn Dialer>> = if key.exists() {
        let key = ClientKey::load(key).map_err(|e| format!("{}: {e}", key.display()))?;
        Some(Arc::new(TlsDialer::new(Arc::new(key))))
    } else {
        None
    };
    Ok(Arc::new(RoutingDialer {
        ipc: ipc_dialer()?,
        tls,
    }))
}

/// A broker ready to run, with its admin channel.
pub struct BrokerParts {
    /// The role.
    pub broker: Broker,
    /// Administrative access to it.
    pub handle: BrokerHandle,
    /// What the admin channel does.
    pub admin: Arc<BrokerAdmin>,
    /// The admin channel's listener (local IPC, owner only).
    pub admin_ipc: Arc<IpcListener>,
    /// Network listeners.
    pub tls: Arc<TlsListening>,
    /// Diagnostics.
    pub logger: Arc<Logger>,
}

/// Builds the broker of `layout`: `policy.toml` checked at start (including `[listen]` and
/// `address = "container"`), `broker.db`, the certificate, the machine registry, IPC for hubs
/// and runners, the admin channel, and TLS when a code is open or a machine is registered.
pub async fn broker(layout: &Layout, foreground: bool) -> Result<BrokerParts, String> {
    crate::init::ensure_state_dir(&layout.root)?;
    layout.prepare_ipc().map_err(|e| e.to_string())?;
    let clock = clock();
    let logger = logger(
        layout,
        &layout.root,
        Role::Broker,
        clock.clone(),
        foreground,
    )?;
    let machine_id = host::machine_id(&layout.root)?;
    let policy_path = layout.config_file("policy.toml");
    let text = read_optional(&policy_path)?.unwrap_or_default();
    let host = host::resources();
    let in_container = host::in_container();
    let (policy, warnings) =
        ActivePolicy::start(&text, host, in_container).map_err(|e| e.to_string())?;
    for w in &warnings {
        logger_warn(&logger, &clock, &w.to_string());
    }
    let addresses = listen_addresses(&policy.file().listen.addresses, in_container)?;
    let access = ipc_access(&policy.file().local_users);
    let store = Arc::new(
        SqliteBrokerStore::open(&layout.root.join("broker.db")).map_err(|e| e.to_string())?,
    );
    let identity = BrokerIdentity::load_or_create(&layout.root).map_err(|e| e.to_string())?;
    let registry = Arc::new(Registry::new(
        store.clone(),
        clock.clone(),
        machine_id.clone(),
        identity.fingerprint(),
    ));
    let desk = Arc::new(crate::join::JoinDesk::new(
        registry.clone(),
        store.clone(),
        clock.clone(),
        machine_id,
    ));
    let ipc = Arc::new(
        IpcListener::bind(&layout.broker_ipc(), access)
            .await
            .map_err(|e| format!("broker IPC {}: {e}", layout.broker_ipc()))?,
    );
    let admin_ipc = Arc::new(
        IpcListener::bind(&layout.admin_ipc(), IpcAccess::OwnerAndAdmins)
            .await
            .map_err(|e| format!("admin IPC {}: {e}", layout.admin_ipc()))?,
    );
    let merged = Arc::new(MergedListener::default());
    merged.add(ipc.clone() as Arc<dyn Listener>);
    let tls = Arc::new(
        TlsListening::new(identity, registry.clone(), merged.clone(), addresses)
            .with_join_desk(desk),
    );
    if registry
        .should_listen(clock.now())
        .await
        .map_err(|e| e.to_string())?
    {
        tls.ensure().await?;
    }
    let owner = current_os_user().map_err(|e| e.to_string())?;
    let broker = Broker::new(BrokerDeps {
        clock: clock.clone(),
        store,
        listener: merged,
        secrets: Arc::new(ConfigSecrets::default()),
        policy,
        meter: logger.metrics(),
    })
    .with_local_admission(Arc::new(
        NetAdmission::new(vec![owner]).with_admins(Arc::new(lr_net::is_machine_admin)),
    ))
    .with_log_sink(logger.clone());
    let handle = broker.handle();
    let admin = Arc::new(BrokerAdmin {
        handle: handle.clone(),
        policy_path,
        logging_path: layout.config_file("logging.toml"),
        registry,
        tls: tls.clone(),
        ipc,
        logger: logger.clone(),
        clock,
        host,
        reloads: tokio::sync::Mutex::new(()),
        stop: Arc::new(tokio::sync::Notify::new()),
        service: layout.is_service(),
    });
    Ok(BrokerParts {
        broker,
        handle,
        admin,
        admin_ipc,
        tls,
        logger,
    })
}

fn logger_warn(logger: &Logger, clock: &Arc<dyn Clock>, message: &str) {
    use lr_abstractions::{Level, LogSink, Record};
    logger.write(&Record::new(
        clock.now(),
        Level::Warn,
        Role::Broker,
        "policy",
        message,
    ));
}

/// A hub ready to run.
pub struct HubParts {
    /// The role.
    pub hub: Hub,
    /// Diagnostics.
    pub logger: Arc<Logger>,
    /// Its CLI channel, closed when the role stops.
    pub ipc: Arc<IpcListener>,
}

/// Whether this machine has a broker of its own for the hub to list as `local`: the broker
/// role written by `init`, the state of a broker that has run here, or the broker service.
pub fn local_broker_configured(layout: &Layout) -> bool {
    crate::init::roles(layout).contains(&crate::init::NodeRole::Broker)
        || layout.root.join("broker.db").exists()
        || layout.broker_is_service()
}

/// Builds the hub of the current OS user: `hub.db`, its CLI IPC, the broker of this machine
/// over IPC when it has one ([`local_broker_configured`]) and joined brokers over TLS, the
/// real git, files and process inspector.
pub async fn hub(layout: &Layout, foreground: bool) -> Result<HubParts, String> {
    crate::init::ensure_state_dir(&layout.root)?;
    layout.prepare_ipc().map_err(|e| e.to_string())?;
    let clock = clock();
    let logger = logger(layout, &layout.root, Role::Hub, clock.clone(), foreground)?;
    let text = read_optional(&layout.config_file("hub-policy.toml"))?.unwrap_or_default();
    let policy = parse_hub_policy(&text).map_err(|e| e.to_string())?;
    let store =
        Arc::new(SqliteHubStore::open(&layout.root.join("hub.db")).map_err(|e| e.to_string())?);
    let principal = current_os_principal().map_err(|e| e.to_string())?;
    let address = layout.hub_ipc(&principal);
    let ipc = Arc::new(
        IpcListener::bind(&address, IpcAccess::OwnerOnly)
            .await
            .map_err(|e| format!("hub IPC {address}: {e}"))?,
    );
    let hub = Hub::new(HubDeps {
        clock,
        store,
        ipc: ipc.clone(),
        dialer: dialer(&layout.root.join(KEY_FILE))?,
        local_broker: local_broker_configured(layout).then(|| layout.broker_ipc()),
        inspector: Arc::new(OsProcessInspector),
        files: Arc::new(StdFiles),
        repository: Arc::new(GitRepository),
        log: logger.clone(),
        meter: logger.metrics(),
        policy,
        name: crate::facts::host_name(),
    })
    .with_logging_reload(logging_reloader(layout, &logger));
    Ok(HubParts { hub, logger, ipc })
}

/// A runner ready to run, with its control channel.
pub struct RunnerParts {
    /// The agent.
    pub agent: RunnerAgent,
    /// Its local control channel (`down`).
    pub control: Arc<IpcListener>,
    /// Diagnostics.
    pub logger: Arc<Logger>,
    /// Problems found while checking `[programs]`.
    pub problems: Vec<String>,
}

/// Where a runner's `runner.toml` is: `explicit`, else its own directory, else the template of
/// the configuration directory.
pub fn runner_config_path(layout: &Layout, name: &str, explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    [
        layout.runner_dir(name).join("runner.toml"),
        layout.config_file("runner.toml"),
    ]
    .into_iter()
    .find(|p| p.exists())
}

#[derive(serde::Deserialize)]
struct BrokerFile {
    address: String,
    fingerprint: Option<String>,
}

fn broker_address(dir: &Path, layout: &Layout) -> Result<BrokerAddress, String> {
    match read_optional(&dir.join(BROKER_FILE))? {
        None => Ok(BrokerAddress {
            address: layout.broker_ipc(),
            fingerprint: None,
        }),
        Some(text) => {
            let f: BrokerFile = toml::from_str(&text).map_err(|e| format!("{BROKER_FILE}: {e}"))?;
            Ok(BrokerAddress {
                address: f.address,
                fingerprint: f.fingerprint,
            })
        }
    }
}

/// Builds runner `name`: `runner.toml` (resources default to the whole host), the elevation
/// guarantee and the refusal to run next to the broker of the same OS user while it admits
/// other clients without `allow_same_user_as_broker` (spec 3.12), program checks, free disk, `runner.db`, real processes, secrets
/// masked in diagnostics, and its control channel.
pub async fn runner(
    layout: &Layout,
    name: &str,
    config: Option<&Path>,
    foreground: bool,
) -> Result<RunnerParts, String> {
    check_runner_name(name)?;
    let dir = layout.runner_dir(name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    layout.prepare_ipc().map_err(|e| e.to_string())?;
    let text = match runner_config_path(layout, name, config) {
        Some(p) => read_optional(&p)?.ok_or_else(|| format!("{} does not exist", p.display()))?,
        None => String::new(),
    };
    let host: HostResources = host::resources();
    let mut cfg = RunnerConfig::parse(&text, Some(name), name, dir.clone(), host)
        .map_err(|e| e.to_string())?;
    for w in &cfg.warnings {
        eprintln!("runner {name}: warning: runner.toml: {w}");
    }
    elevation_allowed(host::is_elevated(), cfg.allow_elevated)
        .map_err(|e| format!("runner {name}: {e}"))?;
    if let Some(risk) = crate::doctor::same_user_verdict(layout, cfg.allow_same_user_as_broker)
        .map_err(|e| format!("runner {name}: {e}"))?
    {
        eprintln!("runner {name}: warning: {risk}");
    }
    cfg.settings.home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok();
    cfg.settings.temp = Some(std::env::temp_dir().to_string_lossy().into_owned());
    let problems = cfg.check_programs();
    cfg.settings.disk_mb = host::free_disk_mb(&dir).unwrap_or(0);
    cfg.settings.disk_ledger = Some(layout.root.join(DISK_LEDGER));
    let pid = std::process::id();
    cfg.settings.process = Some(lr_abstractions::ProcessRef {
        pid,
        start: lr_exec::process_start(pid),
    });
    let clock = clock();
    let logger = logger(layout, &dir, Role::Runner, clock.clone(), foreground)?;
    let store =
        Arc::new(SqliteRunnerStore::open(&dir.join("runner.db")).map_err(|e| e.to_string())?);
    let secrets = RedactingSecrets::new(ConfigSecrets::new(cfg.secrets.clone()), logger.redactor());
    let principal = current_os_principal().map_err(|e| e.to_string())?;
    let address = layout.runner_ipc(&principal, name);
    let control = Arc::new(
        IpcListener::bind(&address, IpcAccess::OwnerOnly)
            .await
            .map_err(|e| format!("runner control IPC {address}: {e}"))?,
    );
    let agent = RunnerAgent::new(RunnerDeps {
        clock: clock.clone(),
        dialer: dialer(&dir.join(KEY_FILE))?,
        broker: broker_address(&dir, layout)?,
        store,
        process: Arc::new(ExecRunner::new(clock)),
        inspector: Arc::new(OsProcessInspector),
        secrets: Arc::new(secrets),
        files: Arc::new(StdFiles),
        log: logger.clone(),
        meter: logger.metrics(),
        settings: cfg.settings,
        disk: Arc::new(host::HostDiskSpace),
        awake: Arc::new(lr_exec::system_keep_awake()),
    });
    Ok(RunnerParts {
        agent,
        control,
        logger,
        problems,
    })
}
