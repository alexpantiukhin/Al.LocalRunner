use crate::error::NetError;
use crate::files::{read_private, write_private};
use crate::fingerprint::Fingerprint;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme};
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

/// File name of the broker certificate in the state directory.
pub const CERT_FILE: &str = "broker-cert.pem";
/// File name of the broker certificate key in the state directory (owner only).
pub const KEY_FILE: &str = "broker-key.pem";

const CERT_NAME: &str = "al-localrunner-broker";

/// The crypto provider of every TLS connection: `ring` (Apache-2.0 AND ISC), chosen over
/// `aws-lc-rs` because it builds without CMake or NASM on every OS.
pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// The broker's self-signed TLS certificate and its key, created on first start (spec 3.8).
/// Nobody validates it against a CA: hubs and runners pin its [`Fingerprint`], which reaches
/// them inside the join code. `Debug` hides the key.
pub struct BrokerIdentity {
    cert: CertificateDer<'static>,
    key_pem: Zeroizing<String>,
}

impl BrokerIdentity {
    /// A new self-signed certificate (ECDSA P-256).
    pub fn generate() -> Result<BrokerIdentity, NetError> {
        let certified = rcgen::generate_simple_self_signed(vec![CERT_NAME.to_string()])
            .map_err(|e| NetError::Tls(e.to_string()))?;
        Ok(BrokerIdentity {
            cert: certified.cert.der().clone(),
            key_pem: Zeroizing::new(certified.key_pair.serialize_pem()),
        })
    }

    /// Loads the certificate and key of `dir`, creating and saving them on the first start.
    /// The key file is written readable by the current OS user only and refused on load when
    /// others can read it; moving the state directory keeps the fingerprint (spec 16.10).
    pub fn load_or_create(dir: &Path) -> Result<BrokerIdentity, NetError> {
        let cert_path = dir.join(CERT_FILE);
        let key_path = dir.join(KEY_FILE);
        if cert_path.exists() || key_path.exists() {
            let cert_pem = std::fs::read(&cert_path)?;
            let cert = CertificateDer::from_pem_slice(&cert_pem)
                .map_err(|e| NetError::Tls(format!("{}: {e}", cert_path.display())))?;
            let key = Zeroizing::new(read_private(&key_path)?);
            let key_pem = Zeroizing::new(
                String::from_utf8(key.to_vec())
                    .map_err(|_| NetError::Tls(format!("{} is not PEM", key_path.display())))?,
            );
            let identity = BrokerIdentity { cert, key_pem };
            identity.private_key()?;
            return Ok(identity);
        }
        let identity = BrokerIdentity::generate()?;
        write_private(&key_path, identity.key_pem.as_bytes())?;
        let cert_pem = pem_of_cert(&identity.cert);
        std::fs::write(&cert_path, cert_pem)?;
        Ok(identity)
    }

    /// The pinned fingerprint of the certificate.
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of_der(&self.cert)
    }

    /// The certificate (DER).
    pub fn certificate(&self) -> &CertificateDer<'static> {
        &self.cert
    }

    fn private_key(&self) -> Result<PrivateKeyDer<'static>, NetError> {
        PrivateKeyDer::from_pem_slice(self.key_pem.as_bytes())
            .map_err(|e| NetError::Tls(format!("certificate key: {e}")))
    }

    /// Server configuration: TLS 1.3 only, no client certificates (participants prove their
    /// ed25519 key by signing a nonce inside the channel).
    pub fn server_config(&self) -> Result<Arc<ServerConfig>, NetError> {
        let config = ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| NetError::Tls(e.to_string()))?
            .with_no_client_auth()
            .with_single_cert(vec![self.cert.clone()], self.private_key()?)
            .map_err(|e| NetError::Tls(e.to_string()))?;
        Ok(Arc::new(config))
    }
}

impl fmt::Debug for BrokerIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrokerIdentity")
            .field("fingerprint", &self.fingerprint())
            .field("key", &"<redacted>")
            .finish()
    }
}

fn pem_of_cert(cert: &CertificateDer<'_>) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    let b64 = STANDARD.encode(cert.as_ref());
    let mut out = String::from("-----BEGIN CERTIFICATE-----\n");
    for line in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).unwrap_or_default());
        out.push('\n');
    }
    out.push_str("-----END CERTIFICATE-----\n");
    out
}

/// Accepts exactly the certificate whose fingerprint is pinned; names, CAs and validity dates
/// play no part. Records what the server presented for the error message.
#[derive(Debug)]
pub(crate) struct PinnedVerifier {
    pin: Fingerprint,
    provider: Arc<CryptoProvider>,
    pub(crate) presented: Mutex<Option<Fingerprint>>,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let presented = Fingerprint::of_der(end_entity);
        *self.presented.lock().unwrap_or_else(|e| e.into_inner()) = Some(presented);
        if presented == self.pin {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Client configuration pinning `pin`, with the verifier to read what the server presented.
pub(crate) fn client_config(
    pin: Fingerprint,
) -> Result<(Arc<ClientConfig>, Arc<PinnedVerifier>), NetError> {
    let provider = provider();
    let verifier = Arc::new(PinnedVerifier {
        pin,
        provider: provider.clone(),
        presented: Mutex::new(None),
    });
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| NetError::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();
    config.enable_sni = false;
    Ok((Arc::new(config), verifier))
}

/// A TLS 1.3 client configuration that accepts only the certificate with fingerprint `pin`.
pub fn pinned_client_config(pin: Fingerprint) -> Result<Arc<ClientConfig>, NetError> {
    Ok(client_config(pin)?.0)
}

/// The fixed server name the client sends; the pin, not the name, identifies the broker.
pub fn server_name() -> ServerName<'static> {
    ServerName::try_from(CERT_NAME).expect("constant is a valid DNS name")
}
