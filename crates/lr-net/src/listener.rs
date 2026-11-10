use crate::auth::{accept_proof, Admit, KeyGate};
use crate::error::NetError;
use crate::fingerprint::Fingerprint;
use crate::listen_addr::ListenAddr;
use crate::slots::{Limits, Slots};
use crate::stream::StreamTransport;
use crate::tls::{channel_binding, BrokerIdentity};
use async_trait::async_trait;
use lr_abstractions::{Listener, Peer, PeerAccess, Transport, TransportError};
use lr_core::Endpoint;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

/// The broker's TLS listener (spec 3.8, 3.9). Every accepted connection has passed TLS with
/// the broker certificate and the signed-nonce key proof; the peer is its source address, the
/// proven key and its [`PeerAccess`]. Handshakes run in background tasks under the
/// [`Limits`], so a slow or flooding peer delays no other.
pub struct TlsListener {
    local: SocketAddr,
    ready: Mutex<mpsc::Receiver<StreamTransport>>,
    task: JoinHandle<()>,
}

struct Shared {
    acceptor: TlsAcceptor,
    own: Fingerprint,
    gate: Arc<dyn KeyGate>,
    slots: Arc<Slots>,
}

impl TlsListener {
    /// Binds `addr` with the default [`Limits`] and starts accepting. Keys are judged by
    /// `gate` after their proof.
    pub async fn bind(
        addr: ListenAddr,
        identity: &BrokerIdentity,
        gate: Arc<dyn KeyGate>,
    ) -> Result<TlsListener, NetError> {
        TlsListener::bind_with(addr, identity, gate, Limits::default()).await
    }

    /// Like [`TlsListener::bind`] with explicit limits.
    pub async fn bind_with(
        addr: ListenAddr,
        identity: &BrokerIdentity,
        gate: Arc<dyn KeyGate>,
        limits: Limits,
    ) -> Result<TlsListener, NetError> {
        let tcp = TcpListener::bind(addr.socket_addr()).await?;
        let local = tcp.local_addr()?;
        let shared = Arc::new(Shared {
            acceptor: TlsAcceptor::from(identity.server_config()?),
            own: identity.fingerprint(),
            gate,
            slots: Slots::new(limits),
        });
        let (tx, rx) = mpsc::channel(16);
        let task = tokio::spawn(accept_loop(tcp, shared, tx));
        Ok(TlsListener {
            local,
            ready: Mutex::new(rx),
            task,
        })
    }

    /// The bound address (the real port when bound to port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }
}

impl Drop for TlsListener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn accept_loop(tcp: TcpListener, shared: Arc<Shared>, tx: mpsc::Sender<StreamTransport>) {
    loop {
        let (stream, from) = match tcp.accept().await {
            Ok(x) => x,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let ip = from.ip().to_canonical();
        let Some(slot) = shared.slots.acquire(ip) else {
            drop(stream);
            continue;
        };
        let (shared, tx) = (shared.clone(), tx.clone());
        tokio::spawn(async move {
            let timeout = shared.slots.limits().handshake_timeout;
            let done = tokio::time::timeout(timeout, handshake(stream, ip, &shared)).await;
            drop(slot);
            if let Ok(Some(transport)) = done {
                let _ = tx.send(transport).await;
            }
        });
    }
}

async fn handshake(
    stream: TcpStream,
    ip: std::net::IpAddr,
    shared: &Shared,
) -> Option<StreamTransport> {
    let _ = stream.set_nodelay(true);
    let mut tls = shared.acceptor.accept(stream).await.ok()?;
    let binding = channel_binding(tls.get_ref().1).ok()?;
    let proof = accept_proof(&mut tls, &shared.own, &binding, shared.gate.as_ref()).await;
    let (key, admit) = match proof {
        Ok(x) => x,
        Err(_) => {
            let _ = tls.shutdown().await;
            return None;
        }
    };
    let access = match admit {
        Admit::Member => {
            shared.slots.remember(ip);
            PeerAccess::Member
        }
        _ => PeerAccess::Joining,
    };
    let peer = Peer {
        endpoint: Endpoint::Addr(ip),
        key: Some(key),
        access,
    };
    let transport = StreamTransport::new(tls, peer);
    Some(match access {
        PeerAccess::Joining => transport.join_only(shared.slots.limits().join_timeout),
        _ => transport,
    })
}

#[async_trait]
impl Listener for TlsListener {
    async fn accept(&self) -> Result<Box<dyn Transport>, TransportError> {
        let mut ready = self.ready.lock().await;
        match ready.recv().await {
            Some(t) => Ok(Box::new(t)),
            None => Err(TransportError::Closed),
        }
    }
}
