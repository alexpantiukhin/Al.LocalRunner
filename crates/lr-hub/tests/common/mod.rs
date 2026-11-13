#![allow(dead_code)]

pub mod broker;

use broker::{FakeBroker, Script};
use lr_abstractions::fakes::{
    FakeListener, FakeNetwork, FakeProcessInspector, ManualClock, MemoryFiles, MemoryHubStore,
    MemoryLogSink, MemoryMeter, MemoryRepository,
};
use lr_abstractions::{
    BrokerLink, Clock, Dialer, Frame, HubStore, Peer, PeerAccess, ProcessRef, Transport,
};
use lr_core::{Endpoint, Identity, JobId, Source, Timestamp, Zone};
use lr_hub::{Hub, HubDeps, HubPolicy};
use lr_proto::{
    decode, CliReply, CliRequest, IpcChannel, IpcOpen, IpcOpenReply, SubmitRequest,
    PROTOCOL_VERSION,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub const START: u64 = 1_700_000_000_000;
pub const HUB_IPC: &str = "hub-ipc";
pub const LOCAL_IPC: &str = "broker-ipc";
pub const SENDER_PID: u32 = 4242;
pub const SENDER_START: u64 = 99;
pub const COMMIT: &str = "abc1234def";
pub const ORIGIN: &str = "https://example.org/acme/app.git";

pub const PROFILES: &str = r#"
[profile.unit]
command = "make test"
timeout_s = 600
mem_mb = 1024

[profile.remote]
command = "make check"
timeout_s = 900
mem_mb = 2048
platforms = ["linux"]

[profile.targeted]
command = "cargo test {? -p {pkg}} {target}"
timeout_s = 600
mem_mb = 1024

[profile.targeted.args.target]
one_of = ["all", "alpha", "beta"]
default = "all"

[profile.targeted.args.pkg]
pattern = "[a-z-]+"

[profile.targeted-remote]
command = "cargo test {target}"
timeout_s = 600
platforms = ["linux"]

[profile.targeted-remote.args.target]
default = "all"
"#;

pub fn repo_root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\work\repo")
    } else {
        PathBuf::from("/work/repo")
    }
}

pub async fn settle() {
    for _ in 0..3000 {
        tokio::task::yield_now().await;
    }
}

pub async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..30000 {
        if f() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("never happened: {what}");
}

async fn yields(n: usize) {
    for _ in 0..n {
        tokio::task::yield_now().await;
    }
}

pub fn identity(label: &str) -> Identity {
    Identity {
        label: label.into(),
        note: None,
        os_user: "alex".into(),
        host: "laptop".into(),
        sender_pid: SENDER_PID,
        sender_start: Some(SENDER_START),
        cwd: repo_root(),
        branch: Some("main".into()),
        worktree_root: Some(repo_root()),
        repo_root: Some(repo_root()),
        project: "acme/app".into(),
        source: Source::Session,
    }
}

pub fn submit(profile: &str, platform: Option<&str>) -> SubmitRequest {
    SubmitRequest {
        identity: identity("agent"),
        profile: Some(profile.into()),
        platform: platform.map(str::to_string),
        timeout_s: None,
        mem_mb: None,
        cpu: None,
        args: Vec::new(),
        detach: false,
        wait: false,
    }
}

fn ipc_peer() -> Peer {
    Peer {
        endpoint: Endpoint::Ipc {
            os_user: "alex".into(),
        },
        key: None,
        access: PeerAccess::Local,
    }
}

pub struct Rig {
    pub clock: ManualClock,
    pub net: FakeNetwork,
    pub ipc: Option<FakeListener>,
    pub store: Arc<MemoryHubStore>,
    pub inspector: FakeProcessInspector,
    pub files: MemoryFiles,
    pub repo: MemoryRepository,
    pub log: Arc<MemoryLogSink>,
    pub meter: Arc<MemoryMeter>,
    pub policy: HubPolicy,
    pub local: bool,
    pub hub_clock: Option<ManualClock>,
}

impl Rig {
    pub fn new() -> Rig {
        let clock = ManualClock::new(Timestamp(START));
        let shared: Arc<dyn Clock> = Arc::new(clock.clone());
        let net = FakeNetwork::new(shared);
        let ipc = net.listen(HUB_IPC, ipc_peer());
        let inspector = FakeProcessInspector::new();
        inspector.add(ProcessRef {
            pid: SENDER_PID,
            start: Some(SENDER_START),
        });
        let files = MemoryFiles::new();
        files.put(
            &repo_root().join(".localrunner").join("profiles.toml"),
            PROFILES.as_bytes(),
        );
        let repo = MemoryRepository::new();
        repo.set_head(&repo_root(), COMMIT, Some(ORIGIN));
        repo.push(COMMIT);
        repo.commit_file(COMMIT, ".localrunner/profiles.toml", PROFILES.as_bytes());
        Rig {
            clock,
            net,
            ipc: Some(ipc),
            store: Arc::new(MemoryHubStore::new()),
            inspector,
            files,
            repo,
            log: Arc::new(MemoryLogSink::new()),
            meter: Arc::new(MemoryMeter::new()),
            policy: HubPolicy::default(),
            local: false,
            hub_clock: None,
        }
    }

    pub fn broker(&self, name: &str, zone: Zone, script: Script) -> FakeBroker {
        let address = format!("{name}:7443");
        FakeBroker::listen(&self.net, &address, zone, script)
    }

    pub fn local_broker(&mut self, script: Script) -> FakeBroker {
        self.local = true;
        FakeBroker::listen(&self.net, LOCAL_IPC, Zone::Local, script)
    }

    pub async fn join(&self, name: &str) {
        self.store
            .upsert_broker(BrokerLink {
                name: name.into(),
                address: format!("{name}:7443"),
                fingerprint: format!("fp-{name}"),
                key_id: "k".into(),
                machine_id: format!("m-{name}"),
                last_zone: None,
            })
            .await
            .unwrap();
    }

    pub fn deps(&mut self) -> HubDeps {
        let dialer: Arc<dyn Dialer> = Arc::new(self.net.dialer(Peer {
            endpoint: Endpoint::Addr("10.0.0.5".parse().unwrap()),
            key: Some("hub-key".into()),
            access: PeerAccess::Member,
        }));
        HubDeps {
            clock: Arc::new(self.hub_clock.clone().unwrap_or_else(|| self.clock.clone())),
            store: self.store.clone(),
            ipc: Arc::new(self.ipc.take().expect("hub started once")),
            dialer,
            local_broker: self.local.then(|| LOCAL_IPC.to_string()),
            inspector: Arc::new(self.inspector.clone()),
            files: Arc::new(self.files.clone()),
            repository: Arc::new(self.repo.clone()),
            log: self.log.clone(),
            meter: self.meter.clone(),
            policy: self.policy.clone(),
            name: "laptop".into(),
        }
    }

    pub fn start(&mut self) -> tokio::task::JoinHandle<()> {
        let hub = Hub::new(self.deps());
        tokio::spawn(hub.run())
    }

    pub async fn restart(
        &mut self,
        old: tokio::task::JoinHandle<()>,
    ) -> tokio::task::JoinHandle<()> {
        old.abort();
        let _ = old.await;
        settle().await;
        self.ipc = Some(self.net.listen(HUB_IPC, ipc_peer()));
        let hub = self.start();
        settle().await;
        hub
    }

    pub async fn cli(&self) -> Cli {
        Cli::open(&self.net, "alex").await
    }

    pub async fn step(&self, secs: u64) {
        for _ in 0..secs {
            self.clock.advance(Duration::from_secs(1));
            settle().await;
        }
    }

    pub async fn pending(&self) -> Vec<(JobId, u64, lr_abstractions::OutboxKind)> {
        self.store
            .pending_outbox()
            .await
            .unwrap()
            .into_iter()
            .map(|e| (e.job, e.seq, e.kind))
            .collect()
    }

    pub async fn active(&self, job: &JobId) -> Option<lr_abstractions::ActiveOrder> {
        self.store.active(job).await.unwrap()
    }

    pub fn log_text(&self) -> String {
        self.log
            .records()
            .iter()
            .map(|r| format!("{:?} {} {:?}", r.level, r.message, r.fields))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub struct Cli {
    pub t: Box<dyn Transport>,
}

impl Cli {
    pub async fn open(net: &FakeNetwork, os_user: &str) -> Cli {
        let dialer = net.dialer(Peer {
            endpoint: Endpoint::Ipc {
                os_user: os_user.into(),
            },
            key: None,
            access: PeerAccess::Local,
        });
        let t = dialer.dial(HUB_IPC, None).await.unwrap();
        let cli = Cli { t };
        cli.send_raw(&IpcOpen::Hello {
            version: PROTOCOL_VERSION,
            channel: IpcChannel::Cli,
        })
        .await;
        let frame = cli.recv_frame().await.expect("open reply");
        let reply: IpcOpenReply = decode(frame.as_bytes()).unwrap();
        assert!(matches!(reply, IpcOpenReply::Ready { .. }), "{reply:?}");
        cli
    }

    pub async fn send_raw<T: serde::Serialize>(&self, msg: &T) {
        self.t.send(Frame::json(msg).unwrap()).await.unwrap();
    }

    pub async fn send(&self, req: CliRequest) {
        self.send_raw(&req).await;
    }

    pub async fn recv_frame(&self) -> Option<Frame> {
        tokio::select! {
            r = self.t.recv() => r.ok(),
            _ = yields(30000) => None,
        }
    }

    pub async fn reply(&self) -> CliReply {
        let frame = self.recv_frame().await.expect("hub replied");
        decode(frame.as_bytes()).unwrap()
    }

    pub async fn submit(&self, req: SubmitRequest) -> JobId {
        self.send(CliRequest::Submit(req)).await;
        match self.reply().await {
            CliReply::Submitted { job, .. } => job,
            other => panic!("expected submitted, got {other:?}"),
        }
    }

    pub async fn close(self) {
        self.t.close().await;
    }
}

pub fn handle_path(job: &JobId) -> PathBuf {
    repo_root()
        .join("temp")
        .join("runner")
        .join(format!("{job}.json"))
}

pub fn log_path(job: &JobId) -> PathBuf {
    repo_root()
        .join("temp")
        .join("runner")
        .join(format!("{job}.log"))
}
