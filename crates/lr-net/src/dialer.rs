use crate::auth::prove;
use crate::fingerprint::Fingerprint;
use crate::keys::ClientKey;
use crate::slots::HANDSHAKE_TIMEOUT;
use crate::stream::StreamTransport;
use crate::tls::{channel_binding, client_config, server_name};
use async_trait::async_trait;
use lr_abstractions::{Dialer, Peer, PeerAccess, Transport, TransportError};
use lr_core::Endpoint;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// Dials brokers over TLS as one participant: pins the broker certificate fingerprint and
/// proves `key` by signing the broker's nonce bound to the TLS session. The resulting peer is
/// the broker's address with the pinned fingerprint as its key and
/// [`PeerAccess::PinnedBroker`].
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
            let binding =
                channel_binding(tls.get_ref().1).map_err(|e| TransportError::Io(e.to_string()))?;
            prove(&mut tls, &pin, &binding, &self.key).await?;
            let peer = Peer {
                endpoint: Endpoint::Addr(remote.ip().to_canonical()),
                key: Some(pin.to_string()),
                access: PeerAccess::PinnedBroker,
            };
            Ok::<Box<dyn Transport>, TransportError>(Box::new(StreamTransport::new(tls, peer)))
        };
        tokio::time::timeout(HANDSHAKE_TIMEOUT, connect)
            .await
            .map_err(|_| TransportError::Unreachable(format!("{address}: handshake timed out")))?
    }
}
