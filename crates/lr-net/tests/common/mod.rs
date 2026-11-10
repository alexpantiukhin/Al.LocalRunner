#![allow(dead_code)]

use lr_abstractions::fakes::MemoryBrokerStore;
use lr_abstractions::{
    BrokerStore, Dialer, Frame, Listener, MachineKind, MachineRow, Transport, TransportError,
};
use lr_core::Timestamp;
use lr_net::{
    BrokerIdentity, ClientKey, CodeRequest, JoinRequest, ListenAddr, Registry, TlsDialer,
    TlsListener,
};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const BROKER_MACHINE_ID: &str = "m-broker";
pub const T0: Timestamp = Timestamp(1_800_000_000_000);

pub fn at_min(min: u64) -> Timestamp {
    T0.plus_secs(min * 60)
}

pub struct Broker {
    pub identity: BrokerIdentity,
    pub store: Arc<MemoryBrokerStore>,
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
    let identity = BrokerIdentity::generate().expect("certificate");
    let store = Arc::new(MemoryBrokerStore::new());
    let dyn_store: Arc<dyn BrokerStore> = store.clone();
    let registry = Arc::new(Registry::new(
        dyn_store,
        BROKER_MACHINE_ID,
        identity.fingerprint(),
    ));
    let addr = ListenAddr::parse("127.0.0.1:0", false).expect("loopback is explicit");
    let listener = TlsListener::bind(addr, &identity, registry.clone())
        .await
        .expect("listener binds");
    let addr = listener.local_addr().to_string();
    Broker {
        identity,
        store,
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
