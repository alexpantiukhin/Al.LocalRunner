use lr_obs::config::parse_endpoint;
use lr_obs::https::HttpsTransport;
use lr_obs::otlp::{ExportError, OtlpTransport, LOGS_PATH};
use rustls as tls;
use std::io::Read;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

struct Pki {
    ca_pem: String,
    cert: tls::pki_types::CertificateDer<'static>,
    key: tls::pki_types::PrivateKeyDer<'static>,
}

fn pki() -> Pki {
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = params.signed_by(&key, &ca, &ca_key).unwrap();
    Pki {
        ca_pem: ca.pem(),
        cert: cert.der().clone(),
        key: tls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
    }
}

fn serve_tls12_only(pki: &Pki, handshaken: Arc<AtomicBool>) -> (u16, std::thread::JoinHandle<()>) {
    let provider = Arc::new(tls::crypto::ring::default_provider());
    let config = tls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&tls::version::TLS12])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![pki.cert.clone()], pki.key.clone_key())
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (tcp, _) = listener.accept().unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let conn = tls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut stream = tls::StreamOwned::new(conn, tcp);
        let mut buf = [0u8; 1024];
        if let Ok(n) = stream.read(&mut buf) {
            if n > 0 {
                handshaken.store(true, Ordering::SeqCst);
            }
        }
    });
    (port, handle)
}

#[test]
fn https_transport_refuses_a_receiver_that_speaks_only_tls_1_2() {
    let pki = pki();
    let handshaken = Arc::new(AtomicBool::new(false));
    let (port, server) = serve_tls12_only(&pki, handshaken.clone());
    let endpoint = parse_endpoint(&format!("https://localhost:{port}")).unwrap();
    let t = HttpsTransport::new(&endpoint, pki.ca_pem.as_bytes(), Duration::from_secs(5)).unwrap();
    assert!(matches!(t.post(LOGS_PATH, b"{}"), Err(ExportError::Io(_))));
    let _ = server.join();
    assert!(
        !handshaken.load(Ordering::SeqCst),
        "no request bytes reached a TLS 1.2 receiver"
    );
}
