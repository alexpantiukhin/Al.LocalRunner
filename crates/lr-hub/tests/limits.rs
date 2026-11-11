mod common;

use async_trait::async_trait;
use broker::Script;
use common::*;
use lr_abstractions::{Dialer, Frame, Level, Listener, Peer, Transport, TransportError};
use lr_core::JobState;
use lr_hub::Hub;
use lr_proto::{CliReply, CliRequest};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

use common::broker;

struct Gated {
    inner: Box<dyn Transport>,
    open: watch::Receiver<bool>,
    greeted: AtomicBool,
}

fn gated(inner: Box<dyn Transport>, open: &watch::Receiver<bool>) -> Box<dyn Transport> {
    Box::new(Gated {
        inner,
        open: open.clone(),
        greeted: AtomicBool::new(false),
    })
}

#[async_trait]
impl Transport for Gated {
    fn peer(&self) -> &Peer {
        self.inner.peer()
    }

    async fn send(&self, frame: Frame) -> Result<(), TransportError> {
        if self.greeted.swap(true, Ordering::SeqCst) {
            let mut open = self.open.clone();
            let _ = open.wait_for(|o| *o).await;
        }
        self.inner.send(frame).await
    }

    async fn recv(&self) -> Result<Frame, TransportError> {
        self.inner.recv().await
    }

    async fn close(&self) {
        self.inner.close().await
    }
}

struct GatedListener {
    inner: Arc<dyn Listener>,
    open: watch::Receiver<bool>,
}

#[async_trait]
impl Listener for GatedListener {
    async fn accept(&self) -> Result<Box<dyn Transport>, TransportError> {
        Ok(gated(self.inner.accept().await?, &self.open))
    }
}

struct GatedDialer {
    inner: Arc<dyn Dialer>,
    open: watch::Receiver<bool>,
}

#[async_trait]
impl Dialer for GatedDialer {
    async fn dial(
        &self,
        address: &str,
        fingerprint: Option<&str>,
    ) -> Result<Box<dyn Transport>, TransportError> {
        Ok(gated(
            self.inner.dial(address, fingerprint).await?,
            &self.open,
        ))
    }
}

fn records(rig: &Rig, message: &str) -> Vec<lr_abstractions::Record> {
    rig.log
        .records()
        .into_iter()
        .filter(|r| r.message == message)
        .collect()
}

fn total(r: &lr_abstractions::Record) -> Option<u64> {
    r.fields
        .iter()
        .find(|(k, _)| k == "total")
        .and_then(|(_, v)| v.parse().ok())
}

#[tokio::test]
async fn replies_beyond_256_per_cli_connection_are_dropped_and_counted() {
    let mut rig = Rig::new();
    let (gate, open) = watch::channel(true);
    let mut deps = rig.deps();
    deps.ipc = Arc::new(GatedListener {
        inner: deps.ipc.clone(),
        open,
    });
    tokio::spawn(Hub::new(deps).run());
    let cli = rig.cli().await;
    gate.send_replace(false);
    for _ in 0..300 {
        cli.send(CliRequest::Jobs {
            label: "agent".into(),
            active: true,
        })
        .await;
    }
    settle().await;
    let dropped = records(&rig, "CLI reply dropped: client not reading");
    assert!(!dropped.is_empty(), "{}", rig.log_text());
    assert_eq!(dropped.last().and_then(total), Some(dropped.len() as u64));
    assert!(dropped.iter().all(|r| r.level == Level::Warn));
    gate.send_replace(true);
    let mut got = 0;
    while let Some(frame) = cli.recv_frame().await {
        let reply: CliReply = lr_proto::decode(frame.as_bytes()).unwrap();
        assert!(matches!(reply, CliReply::Jobs { .. }), "{reply:?}");
        got += 1;
    }
    assert!(
        got >= 256,
        "the hub keeps 256 replies per connection, got {got}"
    );
    assert_eq!(dropped.len() + got, 300);
}

async fn closed_or_hung(cli: &Cli) -> bool {
    loop {
        let next = tokio::select! {
            r = cli.t.recv() => Some(r.is_err()),
            _ = async { for _ in 0..30000 { tokio::task::yield_now().await; } } => None,
        };
        match next {
            Some(true) => return true,
            Some(false) => continue,
            None => return false,
        }
    }
}

#[tokio::test]
async fn slow_wait_watcher_is_disconnected() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    let (gate, open) = watch::channel(true);
    let mut deps = rig.deps();
    deps.ipc = Arc::new(GatedListener {
        inner: deps.ipc.clone(),
        open,
    });
    tokio::spawn(Hub::new(deps).run());
    let job = rig.cli().await.submit(submit("unit", None)).await;
    eventually("accepted", || local.orders().len() == 1).await;
    let watcher = rig.cli().await;
    watcher.send(CliRequest::Wait { job: job.clone() }).await;
    settle().await;
    local.started(&job).await;
    gate.send_replace(false);
    for i in 0..400 {
        local
            .chunk(
                &job,
                &format!(
                    "line {i}
"
                ),
            )
            .await;
    }
    settle().await;
    let dropped = records(
        &rig,
        "slow or closed watcher dropped; the log copy holds the full output",
    );
    assert_eq!(dropped.len(), 1, "{}", rig.log_text());
    gate.send_replace(true);
    assert!(
        closed_or_hung(&watcher).await,
        "the dropped watcher's connection is closed, not left hanging"
    );
}

#[tokio::test]
async fn broker_that_stops_reading_acks_is_dropped_after_1024_frames_and_redialed() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    let (gate, open) = watch::channel(true);
    let mut deps = rig.deps();
    deps.dialer = Arc::new(GatedDialer {
        inner: deps.dialer.clone(),
        open,
    });
    tokio::spawn(Hub::new(deps).run());
    let job = rig.cli().await.submit(submit("unit", None)).await;
    eventually("accepted", || local.orders().len() == 1).await;
    settle().await;
    local.started(&job).await;
    settle().await;
    gate.send_replace(false);
    for i in 0..1100 {
        local.chunk(&job, &format!("line {i}\n")).await;
    }
    settle().await;
    let full = records(&rig, "send queue to broker full, reconnecting");
    assert!(!full.is_empty(), "{}", rig.log_text());
    assert_eq!(full.first().and_then(total), Some(1));
    gate.send_replace(true);
    settle().await;
    rig.step(1).await;
    eventually("redialed", || local.hellos() == 2).await;
}

#[tokio::test]
async fn replay_beyond_1024_frames_waits_for_the_next_second() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner().on_order(broker::OnOrder::Silent));
    let (gate, open) = watch::channel(true);
    let mut deps = rig.deps();
    deps.dialer = Arc::new(GatedDialer {
        inner: deps.dialer.clone(),
        open,
    });
    tokio::spawn(Hub::new(deps).run());
    let cli = rig.cli().await;
    const ORDERS: usize = 1100;
    for _ in 0..ORDERS {
        cli.submit(submit("unit", None)).await;
    }
    eventually("every order sent", || local.orders().len() == ORDERS).await;
    gate.send_replace(false);
    local.disconnect().await;
    settle().await;
    rig.step(1).await;
    eventually("redialed", || local.hellos() == 2).await;
    settle().await;
    let deferred = records(
        &rig,
        "send queue to broker full, the rest waits for the next second",
    );
    assert_eq!(deferred.len(), 1, "{}", rig.log_text());
    assert_eq!(deferred[0].level, Level::Debug);
    assert_eq!(total(&deferred[0]), Some(1));
    gate.send_replace(true);
    settle().await;
    let replayed = local.orders().len() - ORDERS;
    assert!(replayed < ORDERS, "the replay is held at 1024 frames");
    rig.step(1).await;
    eventually("the rest replayed", || local.orders().len() == 2 * ORDERS).await;
    assert_eq!(local.hellos(), 2, "deferral does not reconnect");
    assert!(records(&rig, "send queue to broker full, reconnecting").is_empty());
}

#[tokio::test]
async fn reconnect_pause_starts_at_one_second_and_doubles_to_thirty() {
    let mut rig = Rig::new();
    rig.join("lan-a").await;
    rig.net.set_reachable("lan-a:7443", false);
    let _a = rig.broker(
        "lan-a",
        lr_core::Zone::Lan,
        Script::linux(lr_core::Zone::Lan),
    );
    rig.start();
    settle().await;
    let attempts = |rig: &Rig| records(rig, "broker unreachable").len();
    assert_eq!(attempts(&rig), 1);
    let mut at = Vec::new();
    for second in 1..=95u64 {
        rig.clock.advance(Duration::from_secs(1));
        settle().await;
        if attempts(&rig) > at.len() + 1 {
            at.push(second);
        }
    }
    assert_eq!(at, vec![1, 3, 7, 15, 31, 61, 91]);
}

#[tokio::test]
async fn wait_on_a_job_beyond_the_last_1024_results_is_an_error() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    let mut jobs = Vec::new();
    for _ in 0..1025 {
        let job = cli.submit(submit("unit", None)).await;
        cli.send(CliRequest::Done { job: job.clone() }).await;
        jobs.push(job);
    }
    eventually("all accepted", || local.orders().len() == 1025).await;
    settle().await;
    for job in &jobs {
        local.result(job, JobState::Passed, Some(0)).await;
    }
    settle().await;
    let forgotten = records(
        &rig,
        "finished job forgotten: more than 1024 results in memory",
    );
    assert_eq!(forgotten.len(), 1, "{}", rig.log_text());
    assert_eq!(
        forgotten[0].fields[0],
        ("job".to_string(), jobs[0].0.clone())
    );
    let watcher = rig.cli().await;
    watcher
        .send(CliRequest::Wait {
            job: jobs[1].clone(),
        })
        .await;
    assert!(matches!(watcher.reply().await, CliReply::Exit { .. }));
    watcher
        .send(CliRequest::Wait {
            job: jobs[0].clone(),
        })
        .await;
    assert!(matches!(watcher.reply().await, CliReply::Error(_)));
}

#[tokio::test]
async fn a_successful_connection_to_a_broker_is_logged_with_its_name_only() {
    let mut rig = Rig::new();
    rig.join("lan-a").await;
    let a = rig.broker(
        "lan-a",
        lr_core::Zone::Lan,
        Script::linux(lr_core::Zone::Lan),
    );
    rig.start();
    eventually("hub said hello", || a.hellos() == 1).await;
    eventually("connection logged", || {
        !records(&rig, "broker connected").is_empty()
    })
    .await;
    let connected = records(&rig, "broker connected");
    assert_eq!(connected.len(), 1, "{}", rig.log_text());
    assert_eq!(connected[0].level, Level::Info);
    assert_eq!(
        connected[0].fields,
        vec![("broker".to_string(), "lan-a".to_string())]
    );
}
