#![allow(dead_code)]

pub mod svc;
pub mod wrap;

use lr_abstractions::fakes::{
    FakeDiskSpace, FakeKeepAwake, FakeListener, FakeNetwork, FakeProcessRunner, JobScript,
    ManualClock, MemoryFiles, MemoryLogSink, MemoryMeter, MemoryRunnerStore, MemorySecrets,
};
use lr_abstractions::{
    Clock, Frame, Listener, Peer, PeerAccess, ProcessInspector, RunnerStore, Transport,
};
use lr_core::{Endpoint, JobId, Requirements, StepSpec, Timestamp};
use lr_proto::{
    decode, Ack, Assign, AssignSpec, BrokerToRunner, Hello, ReasonCode, RunnerCancel,
    RunnerToBroker, Sequenced, Welcome, WireCode, PROTOCOL_VERSION,
};
use lr_runner::{AgentControl, AgentExit, BrokerAddress, RunnerAgent, RunnerDeps, RunnerSettings};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use wrap::{CountingStore, Gate, GatedDialer};

pub const BROKER: &str = "broker.example:7443";
pub const FINGERPRINT: &str = "broker-fp";
pub const START: u64 = 1_700_000_000_000;

pub fn state_dir() -> PathBuf {
    PathBuf::from("/state/runner-a")
}

pub struct Rig {
    pub clock: ManualClock,
    pub net: FakeNetwork,
    pub listener: FakeListener,
    pub store: Arc<MemoryRunnerStore>,
    pub process: Arc<FakeProcessRunner>,
    pub files: MemoryFiles,
    pub secrets: Arc<MemorySecrets>,
    pub log: Arc<MemoryLogSink>,
    pub meter: Arc<MemoryMeter>,
    pub settings: RunnerSettings,
    pub gate: Gate,
    pub reads: Arc<AtomicUsize>,
    pub fail_push: Arc<std::sync::atomic::AtomicBool>,
    pub disk: FakeDiskSpace,
    pub awake: FakeKeepAwake,
}

impl Rig {
    pub fn new() -> Rig {
        let clock = ManualClock::new(Timestamp(START));
        let shared: Arc<dyn Clock> = Arc::new(clock.clone());
        let net = FakeNetwork::new(shared.clone());
        let listener = net.listen(
            BROKER,
            Peer {
                endpoint: Endpoint::Addr("10.0.0.1".parse().unwrap()),
                key: Some(FINGERPRINT.into()),
                access: PeerAccess::PinnedBroker,
            },
        );
        let mut settings = RunnerSettings::new("runner-a", state_dir());
        settings.platforms.insert("linux".into());
        settings.path = Some("/usr/bin:/bin".into());
        settings.home = Some("/home/runner".into());
        settings.temp = Some("/tmp/runner".into());
        Rig {
            clock,
            net,
            listener,
            store: Arc::new(MemoryRunnerStore::new()),
            process: Arc::new(FakeProcessRunner::new(shared)),
            files: MemoryFiles::new(),
            secrets: Arc::new(MemorySecrets::new()),
            log: Arc::new(MemoryLogSink::new()),
            meter: Arc::new(MemoryMeter::new()),
            settings,
            gate: Gate::default(),
            reads: Arc::new(AtomicUsize::new(0)),
            fail_push: Arc::default(),
            disk: FakeDiskSpace::new(),
            awake: FakeKeepAwake::new(),
        }
    }

    pub fn outbox_reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    pub fn deps(&self) -> RunnerDeps {
        let dialer = self.net.dialer(Peer {
            endpoint: Endpoint::Addr("10.0.0.2".parse().unwrap()),
            key: Some("runner-key".into()),
            access: PeerAccess::Member,
        });
        let inspector: Arc<dyn ProcessInspector> = Arc::new(self.process.processes());
        RunnerDeps {
            clock: Arc::new(self.clock.clone()),
            dialer: Arc::new(GatedDialer {
                inner: dialer,
                gate: self.gate.clone(),
            }),
            broker: BrokerAddress {
                address: BROKER.into(),
                fingerprint: Some(FINGERPRINT.into()),
            },
            store: Arc::new(CountingStore {
                inner: self.store.clone(),
                reads: self.reads.clone(),
                fail_push: self.fail_push.clone(),
            }),
            process: self.process.clone(),
            inspector,
            secrets: self.secrets.clone(),
            files: Arc::new(self.files.clone()),
            log: self.log.clone(),
            meter: self.meter.clone(),
            settings: self.settings.clone(),
            disk: Arc::new(self.disk.clone()),
            awake: Arc::new(self.awake.clone()),
        }
    }

    pub fn start(&self) -> (AgentControl, JoinHandle<AgentExit>) {
        let agent = RunnerAgent::new(self.deps());
        let control = agent.control();
        (control, tokio::spawn(agent.run()))
    }

    pub fn script(&self, job: &str, script: JobScript) {
        self.process.script(JobId(job.into()), script);
    }

    pub async fn settle(&self) {
        for _ in 0..200 {
            tokio::task::yield_now().await;
        }
    }

    pub async fn advance(&self, by: Duration) {
        self.settle().await;
        self.clock.advance(by);
        self.settle().await;
    }

    pub fn logs_text(&self) -> String {
        self.log
            .records()
            .iter()
            .map(|r| format!("{} {:?}", r.message, r.fields))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub async fn pending(&self) -> Vec<lr_abstractions::OutboxEntry> {
        self.store.pending_outbox().await.unwrap()
    }
}

pub struct Broker {
    pub t: Box<dyn Transport>,
    pub hello: Hello,
    pub auto_ack: bool,
}

impl Broker {
    pub async fn accept(rig: &Rig) -> Broker {
        Self::accept_with(rig, Some(180)).await
    }

    pub async fn accept_with(rig: &Rig, lost_kill: Option<u64>) -> Broker {
        let t = rig.listener.accept().await.unwrap();
        let hello = match decode::<RunnerToBroker>(t.recv().await.unwrap().as_bytes()).unwrap() {
            RunnerToBroker::Hello(h) => h,
            other => panic!("first message must be hello, got {other:?}"),
        };
        let b = Broker {
            t,
            hello,
            auto_ack: true,
        };
        b.send(BrokerToRunner::Welcome(Welcome {
            version: PROTOCOL_VERSION,
            zone: None,
            heartbeat_s: 15,
            broker_lost_kill_s: lost_kill,
        }))
        .await;
        b
    }

    pub async fn send(&self, msg: BrokerToRunner) {
        self.t.send(Frame::json(&msg).unwrap()).await.unwrap();
    }

    pub async fn recv(&self) -> Option<RunnerToBroker> {
        let frame = self.t.recv().await.ok()?;
        Some(decode::<RunnerToBroker>(frame.as_bytes()).unwrap())
    }

    pub async fn next_job_msg(&self) -> Option<RunnerToBroker> {
        loop {
            let msg = self.recv().await?;
            if let Some((job, seq)) = msg.sequence() {
                if self.auto_ack {
                    self.send(BrokerToRunner::Ack(Ack {
                        job: job.clone(),
                        seq,
                    }))
                    .await;
                }
                return Some(msg);
            }
        }
    }

    pub async fn until_result(&self) -> Vec<RunnerToBroker> {
        let mut out = Vec::new();
        while let Some(m) = self.next_job_msg().await {
            let done = matches!(m, RunnerToBroker::JobResult(_));
            out.push(m);
            if done {
                break;
            }
        }
        out
    }

    pub async fn until<F: Fn(&RunnerToBroker) -> bool>(&self, f: F) -> RunnerToBroker {
        loop {
            let m = self.recv().await.expect("connection closed");
            if f(&m) {
                return m;
            }
        }
    }

    pub async fn assign(&self, job: &str, seq: u64, spec: AssignSpec) {
        self.send(BrokerToRunner::Assign(Assign {
            job: JobId(job.into()),
            seq,
            spec: Box::new(spec),
        }))
        .await;
    }

    pub async fn cancel(&self, job: &str, seq: u64, code: ReasonCode) {
        self.send(BrokerToRunner::Cancel(RunnerCancel {
            job: JobId(job.into()),
            seq,
            code,
            detail: Some("by alice".into()),
        }))
        .await;
    }
}

pub fn step(name: &str, command: &str, timeout_s: u64) -> StepSpec {
    StepSpec {
        name: name.into(),
        command: command.into(),
        timeout_s,
        mem_mb: None,
        cpu: None,
        tags: None,
        secrets: Vec::new(),
        continue_on_failure: false,
    }
}

pub fn requirements(timeout_s: u64) -> Requirements {
    Requirements {
        platform: Some("linux".into()),
        timeout_s,
        mem_mb: 0,
        cpu: 0,
        disk_mb: 0,
        programs: Vec::new(),
        tags: Default::default(),
        exclusive: Default::default(),
        same_machine: None,
        services: Vec::new(),
        secrets: Default::default(),
        cache: Vec::new(),
        max_zone: None,
    }
}

pub fn worktree_spec(cwd: &str, steps: Vec<StepSpec>, timeout_s: u64) -> AssignSpec {
    AssignSpec {
        client: "client-1".into(),
        project: "acme/app".into(),
        requirements: requirements(timeout_s),
        steps,
        secrets: Vec::new(),
        code: WireCode::WorkTree { cwd: cwd.into() },
        broker_lost_kill_s: Some(180),
    }
}

pub fn commit_spec(steps: Vec<StepSpec>, timeout_s: u64) -> AssignSpec {
    AssignSpec {
        code: WireCode::Commit {
            sha: "0123456789abcdef0123456789abcdef01234567".into(),
            repo_url: Some("https://git.example.com/acme/app.git".into()),
            branch: Some("main".into()),
        },
        ..worktree_spec("/unused", steps, timeout_s)
    }
}

pub fn result_of(msgs: &[RunnerToBroker]) -> lr_proto::JobResult {
    msgs.iter()
        .find_map(|m| match m {
            RunnerToBroker::JobResult(r) => Some(r.clone()),
            _ => None,
        })
        .expect("a job result")
}

pub fn chunks_text(msgs: &[RunnerToBroker]) -> String {
    msgs.iter()
        .filter_map(|m| match m {
            RunnerToBroker::Chunk(c) => Some(c.text.clone()),
            _ => None,
        })
        .collect()
}
