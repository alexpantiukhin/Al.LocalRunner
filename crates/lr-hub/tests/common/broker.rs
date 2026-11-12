use lr_abstractions::fakes::FakeNetwork;
use lr_abstractions::{Frame, Listener, Peer, PeerAccess, Transport};
use lr_core::{Endpoint, JobId, JobState, MachineRef, Timestamp, Zone};
use lr_proto::{
    decode, Accepted, BrokerToHub, CapabilitiesReply, Chunk, HubCancel, HubToBroker, JobResult,
    Order, ProtocolError, ReasonCode, Rejected, Returned, RunnerCaps, StartedNotice, Welcome,
    WireCapabilities, PROTOCOL_VERSION,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub enum OnOrder {
    Accept,
    Reject(ReasonCode, String),
    Silent,
}

#[derive(Clone)]
pub struct Script {
    pub runners: Vec<RunnerCaps>,
    pub answer_queries: bool,
    pub on_order: OnOrder,
    pub refuse: Option<ProtocolError>,
}

pub fn caps(platforms: &[&str], machine: Option<&str>) -> WireCapabilities {
    WireCapabilities {
        platforms: platforms.iter().map(|p| p.to_string()).collect(),
        mem_mb: 8192,
        cpu: 8,
        disk_mb: 100_000,
        programs: BTreeSet::new(),
        tags: BTreeSet::new(),
        max_job_timeout_s: None,
        services: BTreeMap::new(),
        machine: machine.map(|m| MachineRef(m.into())),
        accept_min_mem_mb: None,
        cache_max_mb: None,
        cache_used_mb: None,
        service_mem_mb: Default::default(),
        commands: Default::default(),
        enforcement: None,
    }
}

impl Script {
    pub fn linux(zone: Zone) -> Script {
        Script {
            runners: vec![RunnerCaps {
                name: "r1".into(),
                online: true,
                zone,
                caps: caps(&["linux"], None),
            }],
            answer_queries: true,
            on_order: OnOrder::Accept,
            refuse: None,
        }
    }

    pub fn local_runner() -> Script {
        Script {
            runners: vec![RunnerCaps {
                name: "local:r1".into(),
                online: true,
                zone: Zone::Local,
                caps: caps(&[], Some("local:alex")),
            }],
            answer_queries: true,
            on_order: OnOrder::Accept,
            refuse: None,
        }
    }

    pub fn on_order(mut self, on: OnOrder) -> Script {
        self.on_order = on;
        self
    }
}

#[derive(Default)]
struct State {
    script: Option<Script>,
    received: Vec<HubToBroker>,
    hellos: usize,
    conn: Option<Arc<dyn Transport>>,
    seq: BTreeMap<JobId, u64>,
    zone: Option<Zone>,
}

#[derive(Clone)]
pub struct FakeBroker {
    state: Arc<Mutex<State>>,
}

fn listener_peer(address: &str) -> Peer {
    if address == super::LOCAL_IPC {
        return Peer {
            endpoint: Endpoint::Ipc {
                os_user: "alex".into(),
            },
            key: None,
            access: PeerAccess::Local,
        };
    }
    let name = address.split(':').next().unwrap_or(address);
    Peer {
        endpoint: Endpoint::Addr("10.0.0.9".parse().unwrap()),
        key: Some(format!("fp-{name}")),
        access: PeerAccess::PinnedBroker,
    }
}

impl FakeBroker {
    pub fn listen(net: &FakeNetwork, address: &str, zone: Zone, script: Script) -> FakeBroker {
        let listener = net.listen(address, listener_peer(address));
        let broker = FakeBroker {
            state: Arc::new(Mutex::new(State {
                script: Some(script),
                zone: Some(zone),
                ..State::default()
            })),
        };
        let b = broker.clone();
        tokio::spawn(async move {
            while let Ok(t) = listener.accept().await {
                let t: Arc<dyn Transport> = Arc::from(t);
                let b = b.clone();
                tokio::spawn(async move { b.serve(t).await });
            }
        });
        broker
    }

    pub fn report_zone(&self, zone: Option<Zone>) {
        self.state.lock().unwrap().zone = zone;
    }

    fn script(&self) -> Script {
        self.state.lock().unwrap().script.clone().unwrap()
    }

    pub fn set(&self, f: impl FnOnce(&mut Script)) {
        f(self.state.lock().unwrap().script.as_mut().unwrap());
    }

    async fn reply(t: &Arc<dyn Transport>, msg: &BrokerToHub) {
        let _ = t.send(Frame::json(msg).unwrap()).await;
    }

    async fn serve(&self, t: Arc<dyn Transport>) {
        let Ok(first) = t.recv().await else { return };
        let Ok(HubToBroker::Hello(_)) = decode::<HubToBroker>(first.as_bytes()) else {
            return;
        };
        let script = self.script();
        if let Some(e) = script.refuse {
            Self::reply(&t, &BrokerToHub::Error(e)).await;
            t.close().await;
            return;
        }
        let zone = self.state.lock().unwrap().zone;
        Self::reply(
            &t,
            &BrokerToHub::Welcome(Welcome {
                version: PROTOCOL_VERSION,
                zone,
                heartbeat_s: 15,
                broker_lost_kill_s: Some(180),
            }),
        )
        .await;
        {
            let mut s = self.state.lock().unwrap();
            s.hellos += 1;
            s.conn = Some(t.clone());
        }
        while let Ok(frame) = t.recv().await {
            let Ok(msg) = decode::<HubToBroker>(frame.as_bytes()) else {
                continue;
            };
            self.state.lock().unwrap().received.push(msg.clone());
            let script = self.script();
            match msg {
                HubToBroker::CapabilitiesQuery(q) if script.answer_queries => {
                    let reply = BrokerToHub::CapabilitiesReply(CapabilitiesReply {
                        req: q.req,
                        zone: zone.unwrap_or(Zone::Remote),
                        runners: script.runners.clone(),
                    });
                    Self::reply(&t, &reply).await;
                }
                HubToBroker::Order(o) => match script.on_order {
                    OnOrder::Accept => {
                        let a = BrokerToHub::Accepted(Accepted {
                            job: o.job.clone(),
                            order_seq: o.seq,
                            waiting: None,
                            timeout_trimmed_to: None,
                        });
                        Self::reply(&t, &a).await;
                    }
                    OnOrder::Reject(code, detail) => {
                        let r = BrokerToHub::Rejected(Rejected {
                            job: o.job.clone(),
                            order_seq: o.seq,
                            code,
                            detail,
                        });
                        Self::reply(&t, &r).await;
                    }
                    OnOrder::Silent => {}
                },
                HubToBroker::Cancel(c) => {
                    let ack = BrokerToHub::Ack(lr_proto::Ack {
                        job: c.job.clone(),
                        seq: c.seq,
                    });
                    Self::reply(&t, &ack).await;
                }
                _ => {}
            }
        }
    }

    pub fn received(&self) -> Vec<HubToBroker> {
        self.state.lock().unwrap().received.clone()
    }

    pub fn orders(&self) -> Vec<Order> {
        self.received()
            .into_iter()
            .filter_map(|m| match m {
                HubToBroker::Order(o) => Some(o),
                _ => None,
            })
            .collect()
    }

    pub fn cancels(&self) -> Vec<HubCancel> {
        self.received()
            .into_iter()
            .filter_map(|m| match m {
                HubToBroker::Cancel(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    pub fn queries(&self) -> usize {
        self.received()
            .iter()
            .filter(|m| matches!(m, HubToBroker::CapabilitiesQuery(_)))
            .count()
    }

    pub fn acks(&self) -> Vec<(JobId, u64)> {
        self.received()
            .into_iter()
            .filter_map(|m| match m {
                HubToBroker::Ack(a) => Some((a.job, a.seq)),
                _ => None,
            })
            .collect()
    }

    pub fn resumed(&self) -> bool {
        self.received()
            .iter()
            .any(|m| matches!(m, HubToBroker::Resume(_)))
    }

    pub fn hellos(&self) -> usize {
        self.state.lock().unwrap().hellos
    }

    pub fn next_seq(&self, job: &JobId) -> u64 {
        let mut s = self.state.lock().unwrap();
        let n = s.seq.entry(job.clone()).or_insert(0);
        *n += 1;
        *n
    }

    pub async fn send(&self, msg: BrokerToHub) {
        let conn = self
            .state
            .lock()
            .unwrap()
            .conn
            .clone()
            .expect("hub connected");
        Self::reply(&conn, &msg).await;
    }

    pub async fn send_frame(&self, frame: Frame) {
        let conn = self
            .state
            .lock()
            .unwrap()
            .conn
            .clone()
            .expect("hub connected");
        let _ = conn.send(frame).await;
    }

    pub async fn started(&self, job: &JobId) {
        let seq = self.next_seq(job);
        self.send(BrokerToHub::Started(StartedNotice {
            job: job.clone(),
            seq,
            runner: "r1".into(),
            started_at: Timestamp(1),
        }))
        .await;
    }

    pub async fn chunk(&self, job: &JobId, text: &str) {
        let seq = self.next_seq(job);
        self.send(BrokerToHub::Chunk(Chunk {
            job: job.clone(),
            seq,
            step: Some(0),
            text: text.into(),
        }))
        .await;
    }

    pub async fn result(&self, job: &JobId, state: JobState, exit_code: Option<i32>) {
        let seq = self.next_seq(job);
        self.send(BrokerToHub::JobResult(JobResult {
            job: job.clone(),
            seq,
            state,
            exit_code,
            reason: None,
            detail: None,
            peak_disk_mb: None,
            peak_mem_mb: None,
        }))
        .await;
    }

    pub async fn returned(&self, job: &JobId, code: ReasonCode) {
        let seq = self.next_seq(job);
        self.send(BrokerToHub::Returned(Returned {
            job: job.clone(),
            seq,
            code,
            detail: Some("runner gone".into()),
        }))
        .await;
    }

    pub async fn disconnect(&self) {
        let conn = self.state.lock().unwrap().conn.take();
        if let Some(c) = conn {
            c.close().await;
        }
    }
}
