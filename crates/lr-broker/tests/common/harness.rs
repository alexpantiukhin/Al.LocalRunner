use super::faulty::FaultyStore;
use super::listeners::{Stall, StallListener};
pub use super::msgs::*;
use lr_abstractions::fakes::{
    FakeListener, FakeNetwork, ManualClock, MemoryBrokerStore, MemoryLogSink, MemoryMeter,
    MemorySecrets,
};
use lr_abstractions::{
    BrokerStore, Dialer, Frame, Join, JoinCodeRow, Listener, MachineKind, MachineRow, Peer,
    PeerAccess, Transport,
};
use lr_broker::policy::{ActivePolicy, HostResources};
use lr_broker::{Broker, BrokerDeps, BrokerHandle, BusEvent, ExactLocalUsers};
use lr_core::{Endpoint, Timestamp};
use lr_proto::*;
use serde::Serialize;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

pub const START: u64 = 1_700_000_000_000;
pub const ADDR: &str = "broker";
pub const POOL: &str = "[pool]\nmem_mb = 16384\ncpu = 8\n";
pub const USERS: &str = "[[local_user]]\nname = \"alex\"\n[[local_user]]\nname = \"bob\"\n\
[[local_user]]\nname = \"root\"\n";
const HOST_RES: HostResources = HostResources {
    mem_mb: 65536,
    cpu: 32,
};
const WAIT: Duration = Duration::from_secs(5);

pub fn policy(extra: &str) -> String {
    format!("{POOL}{USERS}{extra}")
}

pub struct Harness {
    pub clock: ManualClock,
    pub store: Arc<MemoryBrokerStore>,
    pub faults: Arc<FaultyStore>,
    pub log: Arc<MemoryLogSink>,
    pub meter: Arc<MemoryMeter>,
    pub net: FakeNetwork,
    pub events: broadcast::Receiver<BusEvent>,
    pub handle: BrokerHandle,
    task: tokio::task::JoinHandle<()>,
    extra: String,
    owners: Vec<String>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Harness {
    pub fn start(extra: &str) -> Harness {
        Harness::with_owners(extra, &["root"])
    }

    pub fn with_owners(extra: &str, owners: &[&str]) -> Harness {
        Harness::build(extra, owners, |l| Arc::new(l))
    }

    pub fn stalling(extra: &str) -> (Harness, Stall) {
        let stall = Stall::default();
        let s = stall.clone();
        let h = Harness::build(extra, &["root"], move |inner| {
            Arc::new(StallListener { inner, stall: s })
        });
        (h, stall)
    }

    pub fn build(
        extra: &str,
        owners: &[&str],
        wrap: impl FnOnce(FakeListener) -> Arc<dyn Listener>,
    ) -> Harness {
        let clock = ManualClock::new(Timestamp(START));
        Harness::over(
            Arc::new(MemoryBrokerStore::new()),
            clock,
            extra,
            owners,
            wrap,
            |_| {},
        )
    }

    pub async fn restart(&self) -> Harness {
        self.restart_with(|_| {}).await
    }

    pub async fn restart_with(&self, prepare: impl FnOnce(&FaultyStore)) -> Harness {
        self.task.abort();
        let owners: Vec<&str> = self.owners.iter().map(String::as_str).collect();
        let h = Harness::over(
            self.store.clone(),
            self.clock.clone(),
            &self.extra,
            &owners,
            |l| Arc::new(l),
            prepare,
        );
        h.poke().await;
        h
    }

    fn over(
        store: Arc<MemoryBrokerStore>,
        clock: ManualClock,
        extra: &str,
        owners: &[&str],
        wrap: impl FnOnce(FakeListener) -> Arc<dyn Listener>,
        prepare: impl FnOnce(&FaultyStore),
    ) -> Harness {
        let net = FakeNetwork::new(Arc::new(clock.clone()));
        let listener = net.listen(
            ADDR,
            Peer {
                endpoint: Endpoint::Addr(ip("10.0.0.1")),
                key: Some("fp".into()),
                access: PeerAccess::PinnedBroker,
            },
        );
        let faults = Arc::new(FaultyStore::new(store.clone()));
        prepare(&faults);
        let log = Arc::new(MemoryLogSink::new());
        let meter = Arc::new(MemoryMeter::new());
        let (active, _) =
            ActivePolicy::start(&policy(extra), HOST_RES, false).expect("policy loads");
        let broker = Broker::new(BrokerDeps {
            clock: Arc::new(clock.clone()),
            store: faults.clone(),
            listener: wrap(listener),
            secrets: Arc::new(MemorySecrets::new()),
            policy: active,
            meter: meter.clone(),
        })
        .with_local_admission(Arc::new(ExactLocalUsers::with_owners(
            owners.iter().map(|s| s.to_string()),
        )))
        .with_log_sink(log.clone());
        let events = broker.subscribe();
        let handle = broker.handle();
        let task = tokio::spawn(broker.run());
        Harness {
            clock,
            store,
            faults,
            log,
            meter,
            net,
            events,
            handle,
            task,
            extra: extra.to_string(),
            owners: owners.iter().map(|s| s.to_string()).collect(),
        }
    }

    pub async fn dial(&self, peer: Peer) -> Conn {
        let t = self
            .net
            .dialer(peer)
            .dial(ADDR, None)
            .await
            .expect("dial broker");
        Conn {
            t,
            stash: VecDeque::new(),
            req: 0,
        }
    }

    pub async fn seed(&self, name: &str, kind: MachineKind, label: &str) -> String {
        let key = format!("pk-{name}");
        let hash = format!("h-{name}");
        self.store
            .issue_code(JoinCodeRow {
                code_hash: hash.clone(),
                kind,
                name: None,
                label: Some(label.into()),
                uses_left: 1,
                issued_at: Timestamp(0),
                expires_at: Timestamp(u64::MAX),
                redeemed_at: None,
                machine_name: None,
            })
            .await
            .unwrap();
        self.store
            .redeem(Join {
                code_hash: hash,
                at: Timestamp(START),
                machine: MachineRow {
                    name: name.into(),
                    machine_id: format!("mid-{name}"),
                    kind,
                    label: label.into(),
                    public_key: key.clone(),
                    registered_at: Timestamp(START),
                    last_seen: None,
                    revoked_at: None,
                },
            })
            .await
            .unwrap();
        key
    }

    pub async fn connect_local(&self, os_user: &str) -> Conn {
        self.dial(local_peer(os_user)).await
    }

    pub async fn local_hub(&self, os_user: &str) -> Conn {
        let mut c = self.connect_local(os_user).await;
        c.send(&hello(os_user, None)).await;
        c.welcome().await;
        c
    }

    pub async fn member_hub(&self, name: &str, label: &str, addr: &str) -> Conn {
        let key = self.seed(name, MachineKind::Hub, label).await;
        let mut c = self.dial(member_peer(&key, addr)).await;
        c.send(&hello(name, None)).await;
        c.welcome().await;
        c
    }

    pub async fn local_runner(&self, os_user: &str, name: &str, caps: WireCapabilities) -> Conn {
        let mut c = self.connect_local(os_user).await;
        c.send(&hello(name, Some(caps))).await;
        c.welcome().await;
        c
    }

    pub async fn member_runner(&self, name: &str, addr: &str, caps: WireCapabilities) -> Conn {
        let key = self.seed(name, MachineKind::Runner, "").await;
        let mut c = self.dial(member_peer(&key, addr)).await;
        c.send(&hello(name, Some(caps))).await;
        c.welcome().await;
        c
    }

    pub async fn poke(&self) {
        let mut c = self.connect_local("root").await;
        c.send(&hello("root", None)).await;
        c.welcome().await;
    }

    pub async fn until_state(
        &self,
        client: &str,
        job: &str,
        state: lr_core::JobState,
    ) -> lr_abstractions::JobRecord {
        for _ in 0..2000 {
            if let Some(r) = self.job_row(client, job).await {
                if r.job.state == state {
                    return r;
                }
            }
            tokio::task::yield_now().await;
        }
        panic!("job {job} never became {state:?}");
    }

    pub async fn job_row(&self, client: &str, job: &str) -> Option<lr_abstractions::JobRecord> {
        self.store
            .job(&lr_abstractions::JobKey::new(client, job))
            .await
            .unwrap()
    }
}

pub struct Conn {
    t: Box<dyn Transport>,
    stash: VecDeque<Value>,
    req: u64,
}

fn is_heartbeat(v: &Value) -> bool {
    v.get("type").and_then(Value::as_str) == Some("heartbeat")
}

impl Conn {
    pub async fn send<M: Serialize>(&self, m: &M) {
        self.t
            .send(Frame::json(m).unwrap())
            .await
            .expect("send to broker");
    }

    pub async fn send_raw(&self, json: &str) {
        self.t
            .send(Frame::from_bytes(json.as_bytes().to_vec()))
            .await
            .expect("send to broker");
    }

    pub async fn raw(&mut self) -> Option<Value> {
        if let Some(v) = self.stash.pop_front() {
            return Some(v);
        }
        match tokio::time::timeout(WAIT, self.t.recv()).await {
            Ok(Ok(f)) => Some(f.parse().unwrap()),
            Ok(Err(_)) => None,
            Err(_) => panic!("no frame from the broker within {WAIT:?}"),
        }
    }

    pub async fn value(&mut self) -> Value {
        loop {
            let v = self.raw().await.expect("connection open");
            if !is_heartbeat(&v) {
                return v;
            }
        }
    }

    pub async fn hub(&mut self) -> BrokerToHub {
        serde_json::from_value(self.value().await).unwrap()
    }

    pub async fn runner(&mut self) -> BrokerToRunner {
        serde_json::from_value(self.value().await).unwrap()
    }

    pub async fn welcome(&mut self) -> Welcome {
        match self.hub().await {
            BrokerToHub::Welcome(w) => w,
            other => panic!("expected welcome, got {other:?}"),
        }
    }

    pub async fn closed(&mut self) -> bool {
        loop {
            match self.raw().await {
                None => return true,
                Some(v) if is_heartbeat(&v) => continue,
                Some(_) => return false,
            }
        }
    }

    pub async fn read(&mut self, request: ReadRequest) -> ReadReply {
        let (reply, before) = self.call(request).await;
        self.stash.extend(before);
        reply
    }

    pub async fn call(&mut self, request: ReadRequest) -> (ReadReply, Vec<Value>) {
        self.req += 1;
        let req = self.req;
        self.send(&HubToBroker::Read(ReadCall { req, request }))
            .await;
        let mut before = Vec::new();
        loop {
            let v = self.value().await;
            if let Ok(BrokerToHub::ReadReply(a)) = serde_json::from_value::<BrokerToHub>(v.clone())
            {
                if a.req == req {
                    return (a.reply, before);
                }
            }
            before.push(v);
        }
    }

    pub async fn until_runner(&mut self, name: &str, online: bool) {
        for _ in 0..200 {
            if let ReadReply::Runners { runners } = self.read(ReadRequest::Runners).await {
                if runners.iter().any(|r| r.name == name && r.online == online) {
                    return;
                }
            }
            tokio::task::yield_now().await;
        }
        panic!("runner {name} never became online={online}");
    }

    pub async fn sync(&mut self) -> Vec<BrokerToHub> {
        let (_, before) = self.call(ReadRequest::Runners).await;
        before
            .into_iter()
            .map(|v| serde_json::from_value(v).unwrap())
            .collect()
    }

    pub async fn try_value(&mut self) -> Option<Value> {
        if let Some(v) = self.stash.pop_front() {
            return Some(v);
        }
        for _ in 0..200 {
            tokio::task::yield_now().await;
        }
        loop {
            let got = tokio::select! {
                biased;
                r = self.t.recv() => r.ok().map(|f| f.parse::<Value>().unwrap()),
                _ = std::future::ready(()) => return None,
            };
            match got {
                Some(v) if is_heartbeat(&v) => continue,
                other => return other,
            }
        }
    }

    pub async fn quiet(&mut self) {
        if let Some(v) = self.try_value().await {
            panic!("expected nothing, got {v}");
        }
    }

    pub async fn assign(&mut self) -> Assign {
        loop {
            match self.runner().await {
                BrokerToRunner::Assign(a) => return a,
                BrokerToRunner::Ack(_) => continue,
                other => panic!("expected assign, got {other:?}"),
            }
        }
    }

    async fn answer(&mut self) -> BrokerToHub {
        loop {
            match self.hub().await {
                m @ (BrokerToHub::Accepted(_)
                | BrokerToHub::Rejected(_)
                | BrokerToHub::Error(_)) => return m,
                _ => continue,
            }
        }
    }

    pub async fn accepted(&mut self) -> Accepted {
        match self.answer().await {
            BrokerToHub::Accepted(a) => a,
            other => panic!("expected accepted, got {other:?}"),
        }
    }

    pub async fn rejected(&mut self) -> Rejected {
        match self.answer().await {
            BrokerToHub::Rejected(r) => r,
            other => panic!("expected rejected, got {other:?}"),
        }
    }

    pub async fn started_notice(&mut self) -> StartedNotice {
        loop {
            if let BrokerToHub::Started(s) = self.hub().await {
                return s;
            }
        }
    }

    pub async fn acked(&mut self, seq: u64) {
        loop {
            if let BrokerToRunner::Ack(a) = self.runner().await {
                if a.seq == seq {
                    return;
                }
            }
        }
    }

    pub async fn close(&self) {
        self.t.close().await;
    }
}
