mod common;

use common::*;
use lr_abstractions::{Dialer, TransportError};
use lr_net::codec::{read_frame, write_frame};
use lr_net::{channel_binding, provider, BrokerIdentity, Fingerprint, TlsDialer};
use lr_proto::{encode, AuthFromBroker, AuthToBroker, ProtocolErrorCode};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::{ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};

#[derive(Debug)]
struct WrongKey(Arc<CertifiedKey>);

impl ResolvesServerCert for WrongKey {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }
}

fn foreign_signing_key() -> PrivateKeyDer<'static> {
    let pair = rcgen::KeyPair::generate().unwrap();
    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pair.serialize_der()))
}

async fn fake_broker(config: Arc<ServerConfig>) -> (String, Arc<AtomicBool>) {
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap().to_string();
    let reached = Arc::new(AtomicBool::new(false));
    let flag = reached.clone();
    tokio::spawn(async move {
        let acceptor = TlsAcceptor::from(config);
        while let Ok((stream, _)) = tcp.accept().await {
            let Ok(mut tls) = acceptor.accept(stream).await else {
                continue;
            };
            let challenge = AuthFromBroker::Challenge {
                nonce: "A".repeat(43),
            };
            let _ = write_frame(&mut tls, &encode(&challenge).unwrap()).await;
            if read_frame(&mut tls).await.is_ok() {
                flag.store(true, Ordering::SeqCst);
            }
            let _ = write_frame(&mut tls, &encode(&AuthFromBroker::Authenticated).unwrap()).await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    (addr, reached)
}

#[tokio::test]
async fn pinned_certificate_with_another_signing_key_refused() {
    let real = BrokerIdentity::generate().unwrap();
    let signer = provider()
        .key_provider
        .load_private_key(foreign_signing_key())
        .unwrap();
    let stolen = CertifiedKey::new(vec![real.certificate().clone()], signer);
    let config = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(WrongKey(Arc::new(stolen))));
    let (addr, reached) = fake_broker(Arc::new(config)).await;
    let err = TlsDialer::new(key())
        .dial(&addr, Some(&real.fingerprint().to_string()))
        .await
        .err()
        .expect("a copied certificate without its key is refused");
    assert!(
        !matches!(err, TransportError::Unreachable(_)),
        "refused, not unreachable: {err:?}"
    );
    assert!(!reached.load(Ordering::SeqCst), "no proof was sent");
}

fn self_signed() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let c = rcgen::generate_simple_self_signed(vec!["al-localrunner-broker".into()]).unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(c.key_pair.serialize_der()));
    (c.cert.der().clone(), key)
}

#[tokio::test]
async fn dialer_refuses_a_tls12_only_server() {
    let (cert, key_der) = self_signed();
    let pin = Fingerprint::of_der(&cert);
    let config = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS12])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key_der)
        .unwrap();
    let (addr, reached) = fake_broker(Arc::new(config)).await;
    let err = TlsDialer::new(key())
        .dial(&addr, Some(&pin.to_string()))
        .await
        .err()
        .expect("TLS 1.2 refused");
    assert!(!matches!(err, TransportError::Unreachable(_)), "{err:?}");
    assert!(!reached.load(Ordering::SeqCst));
}

#[derive(Debug)]
struct AnyCert;

impl ServerCertVerifier for AnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[tokio::test]
async fn listener_refuses_a_tls12_only_client() {
    let b = broker().await;
    let config = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS12])
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCert))
        .with_no_client_auth();
    let tcp = TcpStream::connect(&b.addr).await.unwrap();
    let name = ServerName::try_from("al-localrunner-broker").unwrap();
    let got = TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await;
    assert!(got.is_err(), "TLS 1.2 handshake must fail");
    assert!(b.nothing_accepted().await);
}

#[tokio::test]
async fn proof_from_one_connection_is_invalid_on_another() {
    let b = broker().await;
    let k = key();
    b.register(&k, "light").await;
    let fp = b.identity.fingerprint();
    let first = raw_tls(&b).await;
    let mut second = raw_tls(&b).await;
    let n2 = challenge(&mut second).await;
    let foreign = channel_binding(first.get_ref().1).unwrap();
    let own = channel_binding(second.get_ref().1).unwrap();
    assert_ne!(foreign, own, "every connection has its own binding");
    let relayed = AuthToBroker::Proof {
        public_key: k.public_key(),
        signature: k.sign_challenge(&fp, &foreign, &n2),
    };
    match send_proof(&mut second, &relayed).await {
        AuthFromBroker::Error(e) => assert_eq!(e.code, ProtocolErrorCode::Unauthorized),
        other => panic!("a proof relayed from another connection was accepted: {other:?}"),
    }
    let mut third = raw_tls(&b).await;
    let n3 = challenge(&mut third).await;
    let own = channel_binding(third.get_ref().1).unwrap();
    let honest = AuthToBroker::Proof {
        public_key: k.public_key(),
        signature: k.sign_challenge(&fp, &own, &n3),
    };
    assert_eq!(
        send_proof(&mut third, &honest).await,
        AuthFromBroker::Authenticated
    );
}
