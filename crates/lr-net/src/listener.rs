use crate::auth::{accept_proof, prove, Admit, KeyGate};
use crate::error::NetError;
use crate::fingerprint::Fingerprint;
use crate::keys::ClientKey;
use crate::stream::StreamTransport;
use crate::tls::{client_config, server_name, BrokerIdentity};
use async_trait::async_trait;
use lr_abstractions::{Dialer, Listener, Peer, Transport, TransportError};
use lr_core::Endpoint;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex, Semaphore};
use tokio::task::JoinHandle;
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// Port of the broker listener when `[listen] address` names none.
pub const DEFAULT_PORT: u16 = 7443;

/// Time a new connection has for TLS and the key proof.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Connections in TLS or key proof at once; more are dropped until one finishes.
pub const MAX_PENDING_HANDSHAKES: usize = 64;

/// A validated `[listen] address` (spec 3.8, 17): an explicit IP address with a port. A
/// wildcard address (`0.0.0.0`, `::`) is refused; the one exception is the value `container`
/// inside a container, which listens on all interfaces of the container at [`DEFAULT_PORT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenAddr(SocketAddr);

impl ListenAddr {
    /// Parses a `[listen] address` value. `in_container` says whether this process runs in a
    /// container; outside one, `container` is refused.
    pub fn parse(value: &str, in_container: bool) -> Result<ListenAddr, NetError> {
        let value = value.trim();
        if value == "container" {
            if !in_container {
                return Err(NetError::BadListenAddress(
                    "`container` is accepted only inside a container".into(),
                ));
            }
            return Ok(ListenAddr(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                DEFAULT_PORT,
            )));
        }
        let addr = value
            .parse::<SocketAddr>()
            .or_else(|_| {
                value
                    .parse::<IpAddr>()
                    .map(|ip| SocketAddr::new(ip, DEFAULT_PORT))
            })
            .map_err(|_| {
                NetError::BadListenAddress(format!(
                    "`{value}` is not an IP address with an optional port"
                ))
            })?;
        ListenAddr::explicit(addr)
    }

    /// An explicit address; a wildcard is refused.
    pub fn explicit(addr: SocketAddr) -> Result<ListenAddr, NetError> {
        if addr.ip().is_unspecified() {
            return Err(NetError::BadListenAddress(format!(
                "wildcard address {} is not allowed; name the interface address",
                addr.ip()
            )));
        }
        Ok(ListenAddr(addr))
    }

    /// The socket address.
    pub fn socket_addr(&self) -> SocketAddr {
        self.0
    }
}

/// The broker's TLS listener (spec 3.8, 3.9). Every accepted connection has passed TLS with
/// the broker certificate and the signed-nonce key proof; the peer is its source address and
/// the proven key. Handshakes run in background tasks, so a slow peer delays no other.
pub struct TlsListener {
    local: SocketAddr,
    ready: Mutex<mpsc::Receiver<StreamTransport>>,
    task: JoinHandle<()>,
}

impl TlsListener {
    /// Binds `addr` and starts accepting. Keys are judged by `gate` after their proof.
    pub async fn bind(
        addr: ListenAddr,
        identity: &BrokerIdentity,
        gate: Arc<dyn KeyGate>,
    ) -> Result<TlsListener, NetError> {
        let tcp = TcpListener::bind(addr.socket_addr()).await?;
        let local = tcp.local_addr()?;
        let acceptor = TlsAcceptor::from(identity.server_config()?);
        let own = identity.fingerprint();
        let (tx, rx) = mpsc::channel(16);
        let task = tokio::spawn(accept_loop(tcp, acceptor, own, gate, tx));
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

async fn accept_loop(
    tcp: TcpListener,
    acceptor: TlsAcceptor,
    own: Fingerprint,
    gate: Arc<dyn KeyGate>,
    tx: mpsc::Sender<StreamTransport>,
) {
    let pending = Arc::new(Semaphore::new(MAX_PENDING_HANDSHAKES));
    loop {
        let (stream, from) = match tcp.accept().await {
            Ok(x) => x,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Ok(permit) = pending.clone().try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let (acceptor, gate, tx) = (acceptor.clone(), gate.clone(), tx.clone());
        tokio::spawn(async move {
            let handshake = handshake(stream, from, acceptor, own, gate);
            if let Ok(Some(transport)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await {
                let _ = tx.send(transport).await;
            }
            drop(permit);
        });
    }
}

async fn handshake(
    stream: TcpStream,
    from: SocketAddr,
    acceptor: TlsAcceptor,
    own: Fingerprint,
    gate: Arc<dyn KeyGate>,
) -> Option<StreamTransport> {
    let _ = stream.set_nodelay(true);
    let mut tls = acceptor.accept(stream).await.ok()?;
    let (key, admit) = match accept_proof(&mut tls, &own, gate.as_ref()).await {
        Ok(x) => x,
        Err(_) => {
            let _ = tls.shutdown().await;
            return None;
        }
    };
    let peer = Peer {
        endpoint: Endpoint::Addr(from.ip().to_canonical()),
        key: Some(key),
    };
    let transport = StreamTransport::new(tls, peer);
    Some(match admit {
        Admit::Joining => transport.join_only(),
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

/// Dials brokers over TLS as one participant: pins the broker certificate fingerprint and
/// proves `key` by signing the broker's nonce. The resulting peer is the broker's address with
/// the pinned fingerprint as its key.
pub struct TlsDialer {
    key: Arc<ClientKey>,
}

impl TlsDialer {
    /// A dialer proving `key` on every connection.
    pub fn new(key: Arc<ClientKey>) -> TlsDialer {
        TlsDialer { key }
    }
}

/// The name of the TLS transport: [`StreamTransport`] over a TLS stream.
pub type TlsTransport = StreamTransport;

#[async_trait]
impl Dialer for TlsDialer {
    async fn dial(
        &self,
        address: &str,
        fingerprint: Option<&str>,
    ) -> Result<Box<dyn Transport>, TransportError> {
        let pin = fingerprint
            .ok_or_else(|| {
                TransportError::Refused("a TLS connection needs the pinned fingerprint".into())
            })
            .and_then(|f| {
                Fingerprint::parse(f).map_err(|e| TransportError::Refused(e.to_string()))
            })?;
        let (config, verifier) =
            client_config(pin).map_err(|e| TransportError::Io(e.to_string()))?;
        let connect = async {
            let tcp = TcpStream::connect(address)
                .await
                .map_err(|e| TransportError::Unreachable(format!("{address}: {e}")))?;
            let _ = tcp.set_nodelay(true);
            let remote = tcp
                .peer_addr()
                .map_err(|e| TransportError::Io(e.to_string()))?;
            let mut tls = match TlsConnector::from(config).connect(server_name(), tcp).await {
                Ok(tls) => tls,
                Err(e) => {
                    let presented = *verifier.presented.lock().unwrap_or_else(|p| p.into_inner());
                    return Err(match presented {
                        Some(got) if got != pin => TransportError::Refused(format!(
                            "wrong_fingerprint: {address} presented certificate {got}, \
                             expected {pin}; the connection was dropped"
                        )),
                        _ => TransportError::Io(format!("tls with {address}: {e}")),
                    });
                }
            };
            prove(&mut tls, &pin, &self.key).await?;
            let peer = Peer {
                endpoint: Endpoint::Addr(remote.ip().to_canonical()),
                key: Some(pin.to_string()),
            };
            Ok::<Box<dyn Transport>, TransportError>(Box::new(StreamTransport::new(tls, peer)))
        };
        tokio::time::timeout(HANDSHAKE_TIMEOUT, connect)
            .await
            .map_err(|_| TransportError::Unreachable(format!("{address}: handshake timed out")))?
    }
}
