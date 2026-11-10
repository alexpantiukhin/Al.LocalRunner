//! Network glue of the assembly: one listener over the broker's IPC and TLS listeners, one
//! dialer that picks IPC or TLS, and admission of local IPC peers by `[[local_user]]`.

use async_trait::async_trait;
use lr_abstractions::{Dialer, Listener, Peer, PeerAccess, Transport, TransportError};
use lr_broker::policy::LocalUserEntry;
use lr_broker::{LocalAdmission, LocalClient};
use lr_core::Endpoint;
use lr_net::{LocalUser, LocalUsers};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

/// Several listeners seen as one; more can be added while it runs (the TLS listener starts when
/// the first join code is issued, spec 3.8).
pub struct MergedListener {
    tx: mpsc::Sender<Result<Box<dyn Transport>, TransportError>>,
    rx: Mutex<mpsc::Receiver<Result<Box<dyn Transport>, TransportError>>>,
    tasks: std::sync::Mutex<Vec<JoinHandle<()>>>,
}

impl Default for MergedListener {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel(16);
        MergedListener {
            tx,
            rx: Mutex::new(rx),
            tasks: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl MergedListener {
    /// Accepts from `inner` too, until it closes.
    pub fn add(&self, inner: Arc<dyn Listener>) {
        let tx = self.tx.clone();
        let task = tokio::spawn(async move {
            loop {
                match inner.accept().await {
                    Ok(t) => {
                        if tx.send(Ok(t)).await.is_err() {
                            return;
                        }
                    }
                    Err(TransportError::Closed) => return,
                    Err(e) => {
                        if tx.send(Err(e)).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(task);
    }
}

impl Drop for MergedListener {
    fn drop(&mut self) {
        for t in self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            t.abort();
        }
    }
}

#[async_trait]
impl Listener for MergedListener {
    async fn accept(&self) -> Result<Box<dyn Transport>, TransportError> {
        self.rx
            .lock()
            .await
            .recv()
            .await
            .unwrap_or(Err(TransportError::Closed))
    }
}

/// Dials local IPC when no fingerprint is given and TLS with the pinned fingerprint otherwise.
pub struct RoutingDialer {
    /// Local IPC.
    pub ipc: Arc<dyn Dialer>,
    /// TLS to joined brokers; `None` before this participant has a key.
    pub tls: Option<Arc<dyn Dialer>>,
}

#[async_trait]
impl Dialer for RoutingDialer {
    async fn dial(
        &self,
        address: &str,
        fingerprint: Option<&str>,
    ) -> Result<Box<dyn Transport>, TransportError> {
        match (fingerprint, &self.tls) {
            (None, _) => self.ipc.dial(address, None).await,
            (Some(f), Some(tls)) => tls.dial(address, Some(f)).await,
            (Some(_), None) => Err(TransportError::Refused(
                "this participant has no key for TLS brokers yet (join first)".into(),
            )),
        }
    }
}

/// Admits local IPC peers named in `[[local_user]]` with the OS rules of `lr-net` (bare Windows
/// names are local accounts of this computer, SIDs allowed), plus the broker owners: the OS
/// users with rights on the broker's state directory.
pub struct NetAdmission {
    owners: Vec<String>,
}

impl NetAdmission {
    /// Admission with these OS users (kernel spelling) as broker owners.
    pub fn new(owners: Vec<String>) -> NetAdmission {
        NetAdmission { owners }
    }
}

fn same(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

impl LocalAdmission for NetAdmission {
    fn admit(&self, os_user: &str, users: &[LocalUserEntry]) -> Option<LocalClient> {
        let list = LocalUsers::new(
            users
                .iter()
                .map(|u| LocalUser {
                    name: u.name.clone(),
                    label: u.label.clone(),
                })
                .collect(),
        );
        let peer = Peer {
            endpoint: Endpoint::Ipc {
                os_user: os_user.to_string(),
            },
            key: None,
            access: PeerAccess::Local,
        };
        let listed = list.admit(&peer).ok();
        let owner = self.owners.iter().any(|o| same(o, os_user));
        if listed.is_none() && !owner {
            return None;
        }
        Some(LocalClient {
            key: format!("local:{os_user}"),
            label: listed.map(|c| c.label).unwrap_or_default(),
            owner,
        })
    }
}
