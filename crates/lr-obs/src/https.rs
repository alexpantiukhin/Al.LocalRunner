//! The real [`OtlpTransport`]: one HTTP/1.1 POST per export over TLS 1.3 (rustls), with the
//! receiver's certificate verified against the CA certificates of `ca_file` only. Connect,
//! every write and every read are bounded by the sink's `timeout_s`; the response head read is
//! bounded to 16 KiB. Errors name the failure, never the request body.

use crate::config::{ConfigError, Endpoint};
use crate::otlp::{ExportError, OtlpTransport};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

const MAX_HEAD: usize = 16 * 1024;

/// Posts OTLP JSON to one endpoint over verified TLS.
pub struct HttpsTransport {
    host: String,
    port: u16,
    base: String,
    config: Arc<ClientConfig>,
    server_name: ServerName<'static>,
    timeout: Duration,
}

fn bad(why: impl Into<String>) -> ConfigError {
    ConfigError(format!("[[sink]] otlp: {}", why.into()))
}

fn io(e: impl std::fmt::Display) -> ExportError {
    ExportError::Io(e.to_string())
}

impl HttpsTransport {
    /// A transport to `endpoint` trusting the PEM certificates in `ca_pem`.
    pub fn new(
        endpoint: &Endpoint,
        ca_pem: &[u8],
        timeout: Duration,
    ) -> Result<HttpsTransport, ConfigError> {
        let mut roots = RootCertStore::empty();
        for cert in CertificateDer::pem_slice_iter(ca_pem) {
            let cert = cert.map_err(|_| bad("ca_file is not valid PEM"))?;
            roots
                .add(cert)
                .map_err(|_| bad("ca_file holds an unusable certificate"))?;
        }
        if roots.is_empty() {
            return Err(bad("ca_file holds no certificate"));
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| bad(e.to_string()))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        let server_name = ServerName::try_from(endpoint.host.clone())
            .map_err(|_| bad("endpoint host is not a valid server name"))?;
        Ok(HttpsTransport {
            host: endpoint.host.clone(),
            port: endpoint.port,
            base: endpoint.path.clone(),
            config: Arc::new(config),
            server_name,
            timeout,
        })
    }

    fn connect(&self) -> Result<TcpStream, ExportError> {
        let addrs = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(io)?;
        let mut last = ExportError::Io("no address".into());
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, self.timeout) {
                Ok(tcp) => {
                    tcp.set_read_timeout(Some(self.timeout)).map_err(io)?;
                    tcp.set_write_timeout(Some(self.timeout)).map_err(io)?;
                    return Ok(tcp);
                }
                Err(e) => last = io(e),
            }
        }
        Err(last)
    }

    fn host_header(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

fn status(head: &[u8]) -> Result<u16, ExportError> {
    let line = head.split(|b| *b == b'\n').next().unwrap_or_default();
    let line = String::from_utf8_lossy(line);
    let mut parts = line.split_whitespace();
    match (
        parts.next(),
        parts.next().and_then(|c| c.parse::<u16>().ok()),
    ) {
        (Some(v), Some(code)) if v.starts_with("HTTP/1.") => Ok(code),
        _ => Err(ExportError::Io("malformed HTTP response".into())),
    }
}

impl OtlpTransport for HttpsTransport {
    fn post(&self, path: &str, body: &[u8]) -> Result<(), ExportError> {
        let tcp = self.connect()?;
        let conn =
            ClientConnection::new(self.config.clone(), self.server_name.clone()).map_err(io)?;
        let mut tls = StreamOwned::new(conn, tcp);
        let head = format!(
            "POST {}{path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.base,
            self.host_header(),
            body.len()
        );
        tls.write_all(head.as_bytes()).map_err(io)?;
        tls.write_all(body).map_err(io)?;
        tls.flush().map_err(io)?;
        let mut got = Vec::new();
        let mut buf = [0u8; 2048];
        while !got.contains(&b'\n') && got.len() < MAX_HEAD {
            match tls.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) if got.is_empty() => return Err(io(e)),
                Err(_) => break,
            }
        }
        let code = status(&got)?;
        tls.conn.send_close_notify();
        let _ = tls.flush();
        if (200..300).contains(&code) {
            Ok(())
        } else {
            Err(ExportError::Status(code))
        }
    }
}
