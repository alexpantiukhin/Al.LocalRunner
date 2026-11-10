#![allow(dead_code)]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use lr_abstractions::fakes::{ManualClock, MemoryBrokerStore};
use lr_abstractions::{
    BrokerStore, Dialer, Frame, Listener, MachineKind, MachineRow, Transport, TransportError,
};
use lr_core::Timestamp;
use lr_net::codec::{read_frame, write_frame};
use lr_net::{pinned_client_config, server_name};
use lr_net::{
    BrokerIdentity, ClientKey, CodeRequest, JoinRequest, Limits, ListenAddr, Registry, TlsDialer,
    TlsListener,
};
use lr_proto::{decode, encode, AuthFromBroker, AuthToBroker};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

pub const BROKER_MACHINE_ID: &str = "m-broker";
pub const T0: Timestamp = Timestamp(1_800_000_000_000);

pub fn at_min(min: u64) -> Timestamp {
    T0.plus_secs(min * 60)
}

pub struct Broker {
    pub identity: BrokerIdentity,
    pub store: Arc<MemoryBrokerStore>,
    pub clock: Arc<ManualClock>,
    pub registry: Arc<Registry>,
    pub listener: TlsListener,
    pub addr: String,
}

impl Broker {
    pub fn fingerprint(&self) -> String {
        self.identity.fingerprint().to_string()
    }

    pub async fn dial(&self, key: &Arc<ClientKey>) -> Result<Box<dyn Transport>, TransportError> {
        TlsDialer::new(key.clone())
            .dial(&self.addr, Some(&self.fingerprint()))
            .await
    }

    pub async fn accept(&self) -> Box<dyn Transport> {
        tokio::time::timeout(Duration::from_secs(10), self.listener.accept())
            .await
            .expect("a connection arrives")
            .expect("accept works")
    }

    pub async fn nothing_accepted(&self) -> bool {
        tokio::time::timeout(Duration::from_millis(300), self.listener.accept())
            .await
            .is_err()
    }

    pub async fn issue(&self, kind: MachineKind, uses: u32) -> String {
        self.registry
            .issue(
                CodeRequest {
                    kind,
                    name: None,
                    label: None,
                    uses,
                    ttl_min: 60,
                },
                T0,
            )
            .await
            .expect("code issued")
            .reveal()
    }

    pub async fn register(&self, key: &ClientKey, name: &str) -> MachineRow {
        let code = self.issue(MachineKind::Runner, 1).await;
        self.registry
            .redeem(join_request(&code, name, key), &key.public_key(), at_min(1))
            .await
            .expect("join recorded")
    }
}

pub fn join_request(code: &str, name: &str, key: &ClientKey) -> JoinRequest {
    JoinRequest {
        code: code.to_string(),
        kind: MachineKind::Runner,
        name: name.to_string(),
        machine_id: format!("m-{name}"),
        public_key: key.public_key(),
    }
}

pub async fn broker() -> Broker {
    broker_with(Limits::default()).await
}

pub async fn broker_with(limits: Limits) -> Broker {
    let identity = BrokerIdentity::generate().expect("certificate");
    let store = Arc::new(MemoryBrokerStore::new());
    let dyn_store: Arc<dyn BrokerStore> = store.clone();
    let clock = Arc::new(ManualClock::new(T0));
    let registry = Arc::new(Registry::new(
        dyn_store,
        clock.clone(),
        BROKER_MACHINE_ID,
        identity.fingerprint(),
    ));
    let addr = ListenAddr::parse("127.0.0.1:0", false).expect("loopback is explicit");
    let listener = TlsListener::bind_with(addr, &identity, registry.clone(), limits)
        .await
        .expect("listener binds");
    let addr = listener.local_addr().to_string();
    Broker {
        identity,
        store,
        clock,
        registry,
        listener,
        addr,
    }
}

pub fn key() -> Arc<ClientKey> {
    Arc::new(ClientKey::generate().expect("key"))
}

pub async fn exchange(a: &dyn Transport, b: &dyn Transport) {
    a.send(Frame::from_bytes(br#"{"type":"heartbeat"}"#.to_vec()))
        .await
        .expect("send");
    let got = b.recv().await.expect("recv");
    assert_eq!(got.as_bytes(), br#"{"type":"heartbeat"}"#);
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

pub fn unique(prefix: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("{prefix}-{}-{nanos}-{n}", std::process::id())
}

pub fn temp_dir() -> tempfile::TempDir {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../temp");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::tempdir_in(base).unwrap()
}

pub async fn raw_tls(b: &Broker) -> TlsStream<TcpStream> {
    let config = pinned_client_config(b.identity.fingerprint()).unwrap();
    let tcp = TcpStream::connect(&b.addr).await.unwrap();
    TlsConnector::from(config)
        .connect(server_name(), tcp)
        .await
        .unwrap()
}

pub async fn challenge(tls: &mut TlsStream<TcpStream>) -> Vec<u8> {
    let msg: AuthFromBroker = decode(&read_frame(tls).await.unwrap()).unwrap();
    let AuthFromBroker::Challenge { nonce } = msg else {
        panic!("expected a challenge, got {msg:?}")
    };
    URL_SAFE_NO_PAD.decode(nonce).unwrap()
}

pub async fn send_proof(tls: &mut TlsStream<TcpStream>, proof: &AuthToBroker) -> AuthFromBroker {
    write_frame(tls, &encode(proof).unwrap()).await.unwrap();
    decode(&read_frame(tls).await.unwrap()).unwrap()
}
