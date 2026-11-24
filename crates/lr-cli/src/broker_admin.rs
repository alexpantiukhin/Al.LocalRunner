//! What the broker's admin channel does (spec 3.8, 3.9, 11.1): policy and logging reload,
//! policy check, join codes, key revocation and the member lists. The network listener starts
//! only while a join code is open or a machine is registered (spec 3.8).

use crate::admin::AdminApi;
use crate::net::MergedListener;
use async_trait::async_trait;
use lr_abstractions::metric::{POLICY_RELOAD_TOTAL, RESULT};
use lr_abstractions::{Clock, Listener, MachineKind, Meter, Peer};
use lr_broker::policy::{check, explain_rules, load, HostResources, ListenValue, LocalUserEntry};
use lr_broker::BrokerHandle;
use lr_net::{
    BrokerIdentity, CodeRequest, IpcAccess, IpcListener, ListenAddr, Registry, TlsListener,
    DEFAULT_PORT,
};
use lr_obs::Logger;
use lr_proto::{AdminReply, AdminRequest};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The broker's network listeners, started on demand.
pub struct TlsListening {
    identity: BrokerIdentity,
    registry: Arc<Registry>,
    merged: Arc<MergedListener>,
    addresses: Mutex<Vec<ListenAddr>>,
    bound: Mutex<Option<Vec<SocketAddr>>>,
    started: Mutex<bool>,
    sockets: Mutex<Vec<(ListenAddr, Arc<TlsListener>)>>,
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
            addresses: Mutex::new(addresses),
            bound: Mutex::new(None),
            started: Mutex::new(false),
            sockets: Mutex::new(Vec::new()),
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
        let addresses = self
            .addresses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if addresses.is_empty() {
            return Err("policy.toml has no [listen] address; the broker cannot be joined".into());
        }
        let mut listeners = Vec::new();
        for addr in &addresses {
            listeners.push((*addr, self.bind(addr).await?));
        }
        let bound: Vec<SocketAddr> = listeners.iter().map(|(_, l)| l.local_addr()).collect();
        for (_, l) in &listeners {
            self.feed(l.clone());
        }
        *self.sockets.lock().unwrap_or_else(|e| e.into_inner()) = listeners;
        *self.bound.lock().unwrap_or_else(|e| e.into_inner()) = Some(bound.clone());
        *self.started.lock().unwrap_or_else(|e| e.into_inner()) = true;
        Ok(bound)
    }

    /// Moves the listeners to `addresses` (the new `[listen]`) without a restart. Before the
    /// broker first started listening, only the addresses of that start change. Once started,
    /// listening goes on through every reload, also after an empty list closed every socket.
    /// Otherwise the new sockets are bound first and accept at once, then the sockets of the
    /// addresses that left stop accepting and let their ports go; an address in both keeps its
    /// socket. Established connections stay. When one address cannot be bound, nothing
    /// changes and the error names it.
    pub async fn rebind(&self, addresses: Vec<ListenAddr>) -> Result<(), String> {
        let _one_at_a_time = self.starting.lock().await;
        let started = *self.started.lock().unwrap_or_else(|e| e.into_inner());
        if !started {
            *self.addresses.lock().unwrap_or_else(|e| e.into_inner()) = addresses;
            return Ok(());
        }
        let old = self
            .sockets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut next = Vec::new();
        let mut fresh = Vec::new();
        for addr in &addresses {
            match old.iter().find(|(a, _)| a == addr) {
                Some((_, l)) => next.push((*addr, l.clone())),
                None => {
                    let l = self.bind(addr).await?;
                    fresh.push(l.clone());
                    next.push((*addr, l));
                }
            }
        }
        for l in fresh {
            self.feed(l);
        }
        let retired: Vec<Arc<TlsListener>> = old
            .into_iter()
            .filter(|(a, _)| !addresses.contains(a))
            .map(|(_, l)| l)
            .collect();
        for l in &retired {
            l.close();
        }
        for l in &retired {
            l.closed().await;
        }
        let bound: Vec<SocketAddr> = next.iter().map(|(_, l)| l.local_addr()).collect();
        *self.sockets.lock().unwrap_or_else(|e| e.into_inner()) = next;
        *self.bound.lock().unwrap_or_else(|e| e.into_inner()) =
            (!bound.is_empty()).then_some(bound);
        *self.addresses.lock().unwrap_or_else(|e| e.into_inner()) = addresses;
        Ok(())
    }

    async fn bind(&self, addr: &ListenAddr) -> Result<Arc<TlsListener>, String> {
        let gate: Arc<dyn lr_net::KeyGate> = self.registry.clone();
        TlsListener::bind(*addr, &self.identity, gate)
            .await
            .map(Arc::new)
            .map_err(|e| format!("listen {}: {e}", addr.socket_addr()))
    }

    fn feed(&self, l: Arc<TlsListener>) {
        let l: Arc<dyn Listener> = l;
        let l: Arc<dyn Listener> = match &self.desk {
            Some(desk) => Arc::new(crate::join::JoiningListener::new(l, desk.clone())),
            None => l,
        };
        self.merged.add(l);
    }

    /// Stops accepting on every network socket at once.
    pub fn close(&self) {
        for (_, l) in self
            .sockets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
        {
            l.close();
        }
    }

    /// Waits until the network sockets closed by [`TlsListening::close`] have let their ports go.
    pub async fn closed(&self) {
        let sockets = self
            .sockets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for (_, l) in sockets {
            l.closed().await;
        }
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
    /// The broker runs inside a container, where alone `[listen] address = "container"` is
    /// accepted.
    pub in_container: bool,
    /// Serializes `policy reload`: the file read, the network listeners, the engine's policy
    /// and the IPC access always change together and in order.
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
    fn reload_failed(&self, message: String) -> AdminReply {
        let labels = [(RESULT, "error")];
        self.logger.metrics().add(POLICY_RELOAD_TOTAL, &labels, 1);
        failed(message)
    }

    async fn reload(&self) -> AdminReply {
        let _in_order = self.reloads.lock().await;
        let text = match read(&self.policy_path) {
            Ok(t) => t,
            Err(e) => return self.reload_failed(e),
        };
        if let Ok(file) = load(&text) {
            let moved = listen_addresses(&file.listen.addresses, self.in_container)
                .map_err(|e| format!("[listen] address: {e}"));
            if let Err(e) = match moved {
                Ok(addresses) => self.tls.rebind(addresses).await,
                Err(e) => Err(e),
            } {
                return self.reload_failed(e);
            }
        }
        match self.handle.reload_policy(&text).await {
            Err(e) => failed(e.to_string()),
            Ok(applied) => {
                let mut warnings: Vec<String> =
                    applied.warnings.iter().map(|w| w.to_string()).collect();
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
            Ok(file) => AdminReply::Policy {
                path: self.policy_path.display().to_string(),
                warnings: check(&file, &self.host)
                    .iter()
                    .map(|w| w.to_string())
                    .collect(),
                rules: explain_rules(&file),
            },
        }
    }

    fn logging_reload(&self) -> AdminReply {
        match crate::build::reload_logging(&self.logging_path, &self.logger) {
            Ok(()) => AdminReply::Ok {
                warnings: Vec::new(),
            },
            Err(e) => failed(e),
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
