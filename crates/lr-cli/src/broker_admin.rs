//! What the broker's admin channel does (spec 3.8, 3.9, 11.1): policy and logging reload,
//! policy check, join codes, key revocation and the member lists. The network listener starts
//! only while a join code is open or a machine is registered (spec 3.8).

use crate::admin::AdminApi;
use crate::net::MergedListener;
use async_trait::async_trait;
use lr_abstractions::{Clock, Listener, MachineKind, Peer};
use lr_broker::policy::{check, load, HostResources, ListenValue, LocalUserEntry};
use lr_broker::BrokerHandle;
use lr_net::{
    BrokerIdentity, CodeRequest, IpcAccess, IpcListener, ListenAddr, Registry, TlsListener,
    DEFAULT_PORT,
};
use lr_obs::{Logger, LoggingConfig};
use lr_proto::{AdminReply, AdminRequest};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The broker's network listeners, started on demand.
pub struct TlsListening {
    identity: BrokerIdentity,
    registry: Arc<Registry>,
    merged: Arc<MergedListener>,
    addresses: Vec<ListenAddr>,
    bound: Mutex<Option<Vec<SocketAddr>>>,
    starting: tokio::sync::Mutex<()>,
    desk: Option<Arc<crate::join::JoinDesk>>,
}

impl TlsListening {
    /// Listeners on `addresses` (from `[listen]`) that feed `merged`.
    pub fn new(
        identity: BrokerIdentity,
        registry: Arc<Registry>,
        merged: Arc<MergedListener>,
        addresses: Vec<ListenAddr>,
    ) -> TlsListening {
        TlsListening {
            identity,
            registry,
            merged,
            addresses,
            bound: Mutex::new(None),
            starting: tokio::sync::Mutex::new(()),
            desk: None,
        }
    }

    /// Joining connections of these listeners go to `desk` (spec 3.8).
    pub fn with_join_desk(mut self, desk: Arc<crate::join::JoinDesk>) -> TlsListening {
        self.desk = Some(desk);
        self
    }

    /// Starts listening if it has not yet; returns the bound addresses in `[listen]` order.
    /// Calls are serialized; when one address fails, none is kept, so a later call retries
    /// them all.
    pub async fn ensure(&self) -> Result<Vec<SocketAddr>, String> {
        let _one_at_a_time = self.starting.lock().await;
        if let Some(b) = self.bound.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return Ok(b);
        }
        if self.addresses.is_empty() {
            return Err("policy.toml has no [listen] address; the broker cannot be joined".into());
        }
        let mut listeners = Vec::new();
        for addr in &self.addresses {
            let gate: Arc<dyn lr_net::KeyGate> = self.registry.clone();
            let l = TlsListener::bind(*addr, &self.identity, gate)
                .await
                .map_err(|e| format!("listen {}: {e}", addr.socket_addr()))?;
            listeners.push(l);
        }
        let bound: Vec<SocketAddr> = listeners.iter().map(|l| l.local_addr()).collect();
        for l in listeners {
            let l: Arc<dyn Listener> = Arc::new(l);
            let l: Arc<dyn Listener> = match &self.desk {
                Some(desk) => Arc::new(crate::join::JoiningListener::new(l, desk.clone())),
                None => l,
            };
            self.merged.add(l);
        }
        *self.bound.lock().unwrap_or_else(|e| e.into_inner()) = Some(bound.clone());
        Ok(bound)
    }

    /// The bound addresses, empty while not listening.
    pub fn bound(&self) -> Vec<SocketAddr> {
        self.bound
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default()
    }

    /// The fingerprint printed with join codes.
    pub fn fingerprint(&self) -> String {
        self.identity.fingerprint().to_string()
    }
}

/// The `[listen]` addresses of a policy as socket addresses.
pub fn listen_addresses(
    values: &[ListenValue],
    in_container: bool,
) -> Result<Vec<ListenAddr>, String> {
    values
        .iter()
        .map(|v| match v {
            ListenValue::Explicit(ip, port) => {
                ListenAddr::explicit(SocketAddr::new(*ip, port.unwrap_or(DEFAULT_PORT)))
            }
            ListenValue::Container => ListenAddr::parse("container", in_container),
        })
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())
}

/// Who may open the broker's IPC: the `[[local_user]]` accounts plus the owner.
pub fn ipc_access(users: &[LocalUserEntry]) -> IpcAccess {
    IpcAccess::Principals(users.iter().map(|u| u.name.clone()).collect())
}

/// The broker's admin commands.
pub struct BrokerAdmin {
    /// The running broker.
    pub handle: BrokerHandle,
    /// `policy.toml`.
    pub policy_path: PathBuf,
    /// `logging.toml`.
    pub logging_path: PathBuf,
    /// Join codes and keys.
    pub registry: Arc<Registry>,
    /// Network listeners.
    pub tls: Arc<TlsListening>,
    /// The broker's IPC, whose access follows `[[local_user]]`.
    pub ipc: Arc<IpcListener>,
    /// Diagnostics of the broker.
    pub logger: Arc<Logger>,
    /// Time source.
    pub clock: Arc<dyn Clock>,
    /// What the host has.
    pub host: HostResources,
    /// Serializes `policy reload`: the file read, the engine's policy and the IPC access
    /// always change together and in order.
    pub reloads: tokio::sync::Mutex<()>,
    /// Signalled by `down` (`shutdown`): the process running this broker stops it.
    pub stop: Arc<tokio::sync::Notify>,
    /// The broker runs under the OS service manager: `down` leaves it to that manager.
    pub service: bool,
}

/// The warning `down` gets from a broker that runs as an OS service and stays.
pub const SERVICE_STAYS: &str =
    "the broker runs as an OS service and keeps running: stop it with `localrunner autostart off broker` (administrator) or the OS service manager";

fn failed(message: impl Into<String>) -> AdminReply {
    AdminReply::Failed {
        message: message.into(),
    }
}

fn read(path: &PathBuf) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

impl BrokerAdmin {
    async fn reload(&self) -> AdminReply {
        let _in_order = self.reloads.lock().await;
        let text = match read(&self.policy_path) {
            Ok(t) => t,
            Err(e) => return failed(e),
        };
        match self.handle.reload_policy(&text).await {
            Err(e) => failed(e.to_string()),
            Ok(applied) => {
                let mut warnings: Vec<String> =
                    applied.warnings.iter().map(|w| w.to_string()).collect();
                for t in &applied.restart_required {
                    warnings.push(format!("{t:?} changes apply after the broker restarts"));
                }
                if let Ok(file) = load(&text) {
                    if let Err(e) = self.ipc.set_access(ipc_access(&file.local_users)).await {
                        warnings.push(format!("IPC access not updated: {e}"));
                    }
                }
                AdminReply::Ok { warnings }
            }
        }
    }

    fn check(&self) -> AdminReply {
        let text = match read(&self.policy_path) {
            Ok(t) => t,
            Err(e) => return failed(e),
        };
        match load(&text) {
            Err(e) => failed(e.to_string()),
            Ok(file) => AdminReply::Ok {
                warnings: check(&file, &self.host)
                    .iter()
                    .map(|w| w.to_string())
                    .collect(),
            },
        }
    }

    fn logging_reload(&self) -> AdminReply {
        let text = match std::fs::read_to_string(&self.logging_path) {
            Ok(t) => Some(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return failed(format!("{}: {e}", self.logging_path.display())),
        };
        match LoggingConfig::load(text.as_deref()).and_then(|c| self.logger.reload(&c)) {
            Ok(()) => AdminReply::Ok {
                warnings: Vec::new(),
            },
            Err(e) => failed(e.to_string()),
        }
    }

    async fn add(
        &self,
        name: Option<String>,
        label: Option<String>,
        runners: Option<u32>,
        address: Option<String>,
    ) -> AdminReply {
        let ttl_min = read(&self.policy_path)
            .ok()
            .and_then(|t| load(&t).ok())
            .map_or(lr_net::DEFAULT_CODE_TTL_MIN, |f| f.join.code_ttl_min.get());
        let bound = match self.tls.ensure().await {
            Ok(b) => b,
            Err(e) => return failed(e),
        };
        let req = CodeRequest {
            kind: if runners.is_some() {
                MachineKind::Runner
            } else {
                MachineKind::Hub
            },
            name,
            label,
            uses: runners.unwrap_or(1),
            ttl_min,
        };
        let now = self.clock.now();
        match self.registry.issue(req, now).await {
            Err(e) => failed(e.to_string()),
            Ok(code) => AdminReply::JoinCode {
                address: address
                    .or_else(|| bound.first().map(|a| a.to_string()))
                    .unwrap_or_default(),
                code: code.reveal(),
                fingerprint: self.tls.fingerprint(),
                expires_at: now.plus_secs(ttl_min.saturating_mul(60)),
            },
        }
    }

    async fn remove(&self, name: &str, kind: MachineKind) -> AdminReply {
        let known = match self.registry.machines().await {
            Ok(m) => m,
            Err(e) => return failed(e.to_string()),
        };
        if !known.iter().any(|m| m.name == name && m.kind == kind) {
            return failed(format!("no {kind:?} named {name}").to_lowercase());
        }
        match self.registry.revoke(name, self.clock.now()).await {
            Err(e) => failed(e.to_string()),
            Ok(_) => {
                self.handle.revoked(name).await;
                AdminReply::Ok {
                    warnings: Vec::new(),
                }
            }
        }
    }

    fn shutdown(&self) -> AdminReply {
        if self.service {
            return AdminReply::Ok {
                warnings: vec![SERVICE_STAYS.to_string()],
            };
        }
        self.stop.notify_one();
        AdminReply::Ok {
            warnings: Vec::new(),
        }
    }

    async fn names(&self, kind: MachineKind) -> AdminReply {
        match self.registry.machines().await {
            Err(e) => failed(e.to_string()),
            Ok(m) => AdminReply::Names {
                names: m
                    .into_iter()
                    .filter(|m| m.kind == kind && m.revoked_at.is_none())
                    .map(|m| m.name)
                    .collect(),
            },
        }
    }
}

#[async_trait]
impl AdminApi for BrokerAdmin {
    async fn handle(&self, _peer: &Peer, req: AdminRequest) -> AdminReply {
        match req {
            AdminRequest::PolicyReload => self.reload().await,
            AdminRequest::PolicyCheck => self.check(),
            AdminRequest::LoggingReload => self.logging_reload(),
            AdminRequest::BrokerAdd {
                name,
                label,
                runners,
                address,
            } => self.add(name, label, runners, address).await,
            AdminRequest::RunnerRemove { name } => self.remove(&name, MachineKind::Runner).await,
            AdminRequest::ClientRemove { name } => self.remove(&name, MachineKind::Hub).await,
            AdminRequest::RunnerList => self.names(MachineKind::Runner).await,
            AdminRequest::ClientList => self.names(MachineKind::Hub).await,
            AdminRequest::Shutdown => self.shutdown(),
        }
    }
}
